//! Read-only check of Recycle Bin settings under which the shell would delete instead of recycle, done before any
//! shell call. A value that cannot be read never blocks; it is reported as a note for the host log.

const POLICIES_EXPLORER: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
const BIT_BUCKET: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket";
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hive {
    CurrentUser,
    LocalMachine,
}

impl Hive {
    fn label(self) -> &'static str {
        match self {
            Hive::CurrentUser => "HKCU",
            Hive::LocalMachine => "HKLM",
        }
    }
}

pub(crate) trait RegistryRead {
    /// `Ok(None)` when the key or the value does not exist. Must never write.
    fn dword(&self, hive: Hive, key: &str, value: &str) -> Result<Option<u32>, String>;
}

/// The bytes one request would move onto a single volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeLoad {
    /// Braced volume GUID, the subkey name under `BitBucket\Volume`.
    pub guid: Option<String>,
    pub bytes: u64,
    /// Volume size available to this user, which per-user disk quotas can reduce.
    pub user_total_bytes: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct SettingsCheck {
    pub verdict: Result<(), String>,
    pub notes: Vec<String>,
}

/// Refuses when policy or the per-volume settings turn recycling off, or when a volume would receive more bytes
/// than its Recycle Bin may hold, because the shell then deletes instead of recycling.
pub(crate) fn check_recycle_settings(
    registry: &dyn RegistryRead,
    volumes: &[VolumeLoad],
) -> SettingsCheck {
    let mut notes = Vec::new();
    let verdict = match refusal(registry, volumes, &mut notes) {
        Some(reason) => Err(reason),
        None => Ok(()),
    };
    SettingsCheck { verdict, notes }
}

fn refusal(
    registry: &dyn RegistryRead,
    volumes: &[VolumeLoad],
    notes: &mut Vec<String>,
) -> Option<String> {
    for hive in [Hive::CurrentUser, Hive::LocalMachine] {
        if read(registry, hive, POLICIES_EXPLORER, "NoRecycleFiles", notes).is_some_and(|v| v != 0)
        {
            return Some(format!(
                "the Recycle Bin is turned off by the NoRecycleFiles policy ({}); nothing was moved",
                hive.label()
            ));
        }
    }
    for hive in [Hive::CurrentUser, Hive::LocalMachine] {
        if read(registry, hive, BIT_BUCKET, "NukeOnDelete", notes).is_some_and(|v| v != 0) {
            return Some(format!(
                "the Recycle Bin is set to delete files immediately (global NukeOnDelete, {}); nothing was moved",
                hive.label()
            ));
        }
    }
    for volume in volumes {
        let Some(guid) = &volume.guid else {
            notes.push(
                "a volume name could not be resolved; its Recycle Bin settings were not checked"
                    .to_string(),
            );
            continue;
        };
        let key = format!(r"{BIT_BUCKET}\Volume\{guid}");
        if read(registry, Hive::CurrentUser, &key, "NukeOnDelete", notes).is_some_and(|v| v != 0) {
            return Some(
                "the Recycle Bin on this drive is set to delete files immediately (NukeOnDelete); nothing was moved"
                    .to_string(),
            );
        }
        let capacity = match read(registry, Hive::CurrentUser, &key, "MaxCapacity", notes) {
            Some(megabytes) => Some(u64::from(megabytes) * MIB),
            None => volume.user_total_bytes.map(default_capacity_bytes),
        };
        match capacity {
            Some(capacity) if volume.bytes > capacity => {
                return Some(format!(
                    "this session ({} MiB) is larger than the Recycle Bin on its drive may hold ({} MiB); nothing was moved",
                    volume.bytes.div_ceil(MIB),
                    capacity / MIB
                ));
            }
            Some(_) => {}
            None => notes.push(format!(
                "the Recycle Bin size of volume {guid} is unknown; the size check was skipped"
            )),
        }
    }
    None
}

fn read(
    registry: &dyn RegistryRead,
    hive: Hive,
    key: &str,
    value: &str,
    notes: &mut Vec<String>,
) -> Option<u32> {
    registry.dword(hive, key, value).unwrap_or_else(|err| {
        notes.push(format!(
            r"could not read {}\{key}\{value}: {err}; not treated as blocking",
            hive.label()
        ));
        None
    })
}

/// Windows Vista and later size an unconfigured bin at 10% of the first 40 GB of the user's quota plus 5% of
/// the rest (The Old New Thing, 2014-07-01).
fn default_capacity_bytes(user_total_bytes: u64) -> u64 {
    let first = user_total_bytes.min(40 * GIB);
    first / 10 + (user_total_bytes - first) / 20
}

/// Reads the live registry through `RegGetValueW`, which only queries values.
pub(crate) struct SystemRegistry;

impl RegistryRead for SystemRegistry {
    fn dword(&self, hive: Hive, key: &str, value: &str) -> Result<Option<u32>, String> {
        use windows_sys::Win32::Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
        };
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD,
        };

        let root = match hive {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        };
        let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: both names are NUL-terminated and outlive the call; `data` is a writable DWORD whose size is
        // passed in `size`, and RRF_RT_REG_DWORD rejects any value that would not fit.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u32).cast(),
                &mut size,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(Some(data)),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
            code => Err(std::io::Error::from_raw_os_error(code as i32).to_string()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Answers from a closure so each test states only the values it cares about.
    pub(crate) struct FakeRegistry<F>(pub F);

    impl<F> RegistryRead for FakeRegistry<F>
    where
        F: Fn(Hive, &str, &str) -> Result<Option<u32>, String>,
    {
        fn dword(&self, hive: Hive, key: &str, value: &str) -> Result<Option<u32>, String> {
            (self.0)(hive, key, value)
        }
    }

    const GUID: &str = "{4a8025de-5eee-4fbc-86e0-eabf513c79a5}";

    fn volume(bytes: u64) -> VolumeLoad {
        VolumeLoad {
            guid: Some(GUID.to_string()),
            bytes,
            user_total_bytes: Some(100 * GIB),
        }
    }

    fn volume_key() -> String {
        format!(r"{BIT_BUCKET}\Volume\{GUID}")
    }

    fn check_with(
        answer: impl Fn(Hive, &str, &str) -> Result<Option<u32>, String>,
        volumes: &[VolumeLoad],
    ) -> SettingsCheck {
        check_recycle_settings(&FakeRegistry(answer), volumes)
    }

    #[test]
    fn nothing_configured_allows_the_move() {
        let check = check_with(|_, _, _| Ok(None), &[volume(10 * MIB)]);
        assert_eq!(check.verdict, Ok(()));
        assert!(check.notes.is_empty(), "{:?}", check.notes);
    }

    #[test]
    fn no_recycle_files_policy_in_either_hive_refuses() {
        for blocked in [Hive::CurrentUser, Hive::LocalMachine] {
            let check = check_with(
                |hive, key, value| {
                    Ok(
                        (hive == blocked && key == POLICIES_EXPLORER && value == "NoRecycleFiles")
                            .then_some(1),
                    )
                },
                &[volume(1)],
            );
            let err = check.verdict.unwrap_err();
            assert!(
                err.contains("NoRecycleFiles") && err.contains(blocked.label()),
                "{err}"
            );
        }
        let disabled = check_with(
            |_, key, value| Ok((key == POLICIES_EXPLORER && value == "NoRecycleFiles").then_some(0)),
            &[volume(1)],
        );
        assert_eq!(disabled.verdict, Ok(()));
    }

    #[test]
    fn global_nuke_on_delete_refuses() {
        for blocked in [Hive::CurrentUser, Hive::LocalMachine] {
            let check = check_with(
                |hive, key, value| {
                    Ok(
                        (hive == blocked && key == BIT_BUCKET && value == "NukeOnDelete")
                            .then_some(1),
                    )
                },
                &[volume(1)],
            );
            assert!(check.verdict.unwrap_err().contains("global NukeOnDelete"));
        }
    }

    #[test]
    fn volume_nuke_on_delete_refuses_only_for_that_volume() {
        let answer = |hive: Hive, key: &str, value: &str| {
            Ok(
                (hive == Hive::CurrentUser && key == volume_key() && value == "NukeOnDelete")
                    .then_some(1),
            )
        };
        let err = check_with(answer, &[volume(1)]).verdict.unwrap_err();
        assert!(err.contains("on this drive"), "{err}");
        let other = VolumeLoad {
            guid: Some("{00000000-0000-0000-0000-000000000000}".to_string()),
            ..volume(1)
        };
        assert_eq!(check_with(answer, &[other]).verdict, Ok(()));
    }

    #[test]
    fn a_session_larger_than_max_capacity_refuses() {
        let answer = |_: Hive, key: &str, value: &str| {
            Ok((key == volume_key() && value == "MaxCapacity").then_some(1))
        };
        assert_eq!(check_with(answer, &[volume(MIB)]).verdict, Ok(()));
        let err = check_with(answer, &[volume(MIB + 1)]).verdict.unwrap_err();
        assert!(err.contains("(2 MiB)") && err.contains("(1 MiB)"), "{err}");
        let split = [
            volume(MIB),
            VolumeLoad {
                guid: None,
                ..volume(5 * MIB)
            },
        ];
        let check = check_with(answer, &split);
        assert_eq!(check.verdict, Ok(()));
        assert!(check.notes[0].contains("could not be resolved"));
    }

    #[test]
    fn missing_max_capacity_falls_back_to_the_default_size() {
        assert_eq!(default_capacity_bytes(10 * GIB), GIB);
        assert_eq!(default_capacity_bytes(100 * GIB), 4 * GIB + 3 * GIB);
        let check = check_with(|_, _, _| Ok(None), &[volume(7 * GIB + 1)]);
        assert!(check.verdict.unwrap_err().contains("may hold"));
        let unknown = VolumeLoad {
            user_total_bytes: None,
            ..volume(u64::MAX)
        };
        let check = check_with(|_, _, _| Ok(None), &[unknown]);
        assert_eq!(check.verdict, Ok(()));
        assert!(check.notes[0].contains("size check was skipped"));
    }

    #[test]
    fn unreadable_values_are_noted_and_never_block() {
        let check = check_with(
            |_, _, _| Err("Access is denied.".to_string()),
            &[volume(MIB)],
        );
        assert_eq!(check.verdict, Ok(()));
        assert_eq!(check.notes.len(), 6, "{:?}", check.notes);
        assert!(check
            .notes
            .iter()
            .all(|note| note.contains("not treated as blocking")));
        assert!(check.notes[0].starts_with(r"could not read HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\Explorer\NoRecycleFiles"));
    }
}
