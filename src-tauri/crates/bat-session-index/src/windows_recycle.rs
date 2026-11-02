//! Moves items to the Windows Recycle Bin and refuses any delete that the shell would not recycle.

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use windows::core::{implement, ComObject, Ref, HRESULT, PCWSTR};
use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl,
    IShellItem, SHCreateItemFromParsingName, FILEOPERATION_FLAGS, FOFX_EARLYFAILURE,
    FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI,
    FOF_SILENT, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};

use crate::diagnostics::diagnostic;
use crate::trash::measure;
use crate::windows_recycle_settings::{
    check_recycle_settings, RegistryRead, SystemRegistry, VolumeLoad,
};

/// No UI of any kind. FOF_WANTNUKEWARNING must stay out: it partially overrides FOF_NOCONFIRMATION and shows a
/// permanent-delete prompt that a user could accept.
const RECYCLE_FLAGS: FILEOPERATION_FLAGS = FILEOPERATION_FLAGS(
    FOF_SILENT.0
        | FOF_NOCONFIRMATION.0
        | FOF_NOERRORUI.0
        | FOF_NOCONFIRMMKDIR.0
        | FOF_ALLOWUNDO.0
        | FOFX_RECYCLEONDELETE.0
        | FOFX_EARLYFAILURE.0,
);

/// Moves `paths` in order with one IFileOperation on a dedicated STA thread and stops at the first failure.
/// Every path must be absolute and on a fixed local volume whose Recycle Bin settings allow recycling them.
pub(crate) fn recycle(paths: &[PathBuf]) -> Result<(), String> {
    recycle_checked(paths, &SystemRegistry)
}

fn recycle_checked(paths: &[PathBuf], registry: &dyn RegistryRead) -> Result<(), String> {
    let mut names = Vec::with_capacity(paths.len());
    let mut loads: Vec<(Vec<u16>, u64)> = Vec::new();
    for path in paths {
        let root = recycle_bin_volume(path)?;
        names.push(shell_parsing_name(path)?);
        let bytes = measure(std::slice::from_ref(path)).total_bytes;
        match loads.iter_mut().find(|(known, _)| *known == root) {
            Some((_, total)) => *total += bytes,
            None => loads.push((root, bytes)),
        }
    }
    let volumes: Vec<VolumeLoad> = loads
        .iter()
        .map(|(root, bytes)| VolumeLoad {
            guid: volume_guid(root),
            bytes: *bytes,
            user_total_bytes: user_total_bytes(root),
        })
        .collect();
    let check = check_recycle_settings(registry, &volumes);
    for note in &check.notes {
        diagnostic(&format!("recycle bin settings: {note}"));
    }
    check.verdict?;
    std::thread::spawn(move || recycle_on_sta(&names))
        .join()
        .unwrap_or_else(|_| Err("recycle worker panicked".to_string()))
}

fn recycle_on_sta(names: &[Vec<u16>]) -> Result<(), String> {
    // SAFETY: called once on a thread this module owns; the guard below pairs it with CoUninitialize.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if init.is_err() {
        return Err(format!("COM initialization failed: {init}"));
    }
    let _com = ComApartment;
    let sink = ComObject::new(RecycleOnlySink::default());
    let performed = perform_delete(names, &sink.to_interface());
    outcome(performed, sink.report(), names.len())
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: only constructed after CoInitializeEx succeeded on this thread, and every COM object created
        // in `recycle_on_sta` is declared after it and therefore released first.
        unsafe { CoUninitialize() };
    }
}

/// Returns whether the shell reports any aborted operation.
fn perform_delete(
    names: &[Vec<u16>],
    sink: &IFileOperationProgressSink,
) -> windows::core::Result<bool> {
    // SAFETY: COM is initialized on this thread, and each name is a NUL-terminated UTF-16 buffer that outlives
    // the call that reads it.
    unsafe {
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        operation.SetOperationFlags(RECYCLE_FLAGS)?;
        let cookie = operation.Advise(sink)?;
        let result = (|| {
            for name in names {
                let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None)?;
                operation.DeleteItem(&item, None)?;
            }
            operation.PerformOperations()?;
            operation
                .GetAnyOperationsAborted()
                .map(|aborted| aborted.as_bool())
        })();
        let _ = operation.Unadvise(cookie);
        result
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SinkReport {
    vetoed: u32,
    destroyed: u32,
    recycled: u32,
    failed: Option<HRESULT>,
}

/// Cancels the whole operation before any item that the shell is not going to recycle, and flags an item that
/// was deleted without a Recycle Bin copy. `#[implement]` makes the sink agile, so its state is atomic.
#[implement(IFileOperationProgressSink)]
#[derive(Default)]
struct RecycleOnlySink {
    vetoed: AtomicU32,
    destroyed: AtomicU32,
    recycled: AtomicU32,
    /// The last failing HRESULT; 0 (S_OK) means none, since only failures are stored.
    failed: AtomicI32,
}

impl RecycleOnlySink {
    fn report(&self) -> SinkReport {
        let failed = self.failed.load(Ordering::SeqCst);
        SinkReport {
            vetoed: self.vetoed.load(Ordering::SeqCst),
            destroyed: self.destroyed.load(Ordering::SeqCst),
            recycled: self.recycled.load(Ordering::SeqCst),
            failed: (failed != 0).then_some(HRESULT(failed)),
        }
    }
}

fn bump(counter: &AtomicU32) {
    let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1));
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for RecycleOnlySink_Impl {
    /// The shell omits TSF_DELETE_RECYCLE_IF_POSSIBLE for an item it would destroy instead of recycling; an
    /// error here cancels this delete and every pending operation.
    fn PreDeleteItem(&self, dwflags: u32, _item: Ref<'_, IShellItem>) -> windows::core::Result<()> {
        if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            bump(&self.vetoed);
            return Err(E_ABORT.into());
        }
        Ok(())
    }

    /// A successful delete without a newly created Recycle Bin item means the item was destroyed.
    fn PostDeleteItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        hrdelete: HRESULT,
        newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        if hrdelete.is_err() {
            self.failed.store(hrdelete.0, Ordering::SeqCst);
            return Ok(());
        }
        if newly_created.is_null() {
            bump(&self.destroyed);
            return Err(E_ABORT.into());
        }
        bump(&self.recycled);
        Ok(())
    }

    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _hrresult: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreRenameItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _hrrename: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _hrmove: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _hrcopy: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreNewItem(
        &self,
        _dwflags: u32,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _dwflags: u32,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _template_name: &PCWSTR,
        _attributes: u32,
        _hrnew: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn UpdateProgress(&self, _total: u32, _so_far: u32) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Success requires a clean run in which every requested item produced a Recycle Bin item.
fn outcome(
    performed: windows::core::Result<bool>,
    report: SinkReport,
    expected: usize,
) -> Result<(), String> {
    if report.destroyed > 0 {
        return Err(format!(
            "the shell deleted {} item(s) without placing them in the Recycle Bin",
            report.destroyed
        ));
    }
    if report.vetoed > 0 {
        return Err(
            "the Recycle Bin cannot take this item; it was left in place instead of being deleted"
                .to_string(),
        );
    }
    if let Some(hr) = report.failed {
        return Err(format!(
            "moving an item to the Recycle Bin failed: {}",
            windows::core::Error::from_hresult(hr)
        ));
    }
    match performed {
        Err(err) => Err(format!("moving to the Recycle Bin failed: {err}")),
        Ok(true) => Err("the shell aborted the move to the Recycle Bin".to_string()),
        Ok(false) if (report.recycled as usize) < expected => Err(format!(
            "only {} of {expected} items were confirmed in the Recycle Bin",
            report.recycled
        )),
        Ok(false) => Ok(()),
    }
}

/// The shell rejects verbatim (`\\?\`) paths, so those are converted back to ordinary drive or UNC form.
fn shell_parsing_name(path: &Path) -> Result<Vec<u16>, String> {
    if !path.is_absolute() {
        return Err("the Recycle Bin needs an absolute path".to_string());
    }
    let mut components = path.components();
    let mut name = OsString::new();
    match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                name.push(format!("{}:", char::from(letter)));
            }
            Prefix::UNC(host, share) | Prefix::VerbatimUNC(host, share) => {
                name.push(r"\\");
                name.push(host);
                name.push(r"\");
                name.push(share);
            }
            _ => return Err("the Recycle Bin is not available for device paths".to_string()),
        },
        _ => return Err("the Recycle Bin needs a path with a drive or share".to_string()),
    }
    let rest: Vec<_> = components
        .filter(|component| !matches!(component, Component::RootDir))
        .map(Component::as_os_str)
        .collect();
    for part in rest {
        name.push(r"\");
        name.push(part);
    }
    Ok(name.encode_wide().chain(Some(0)).collect())
}

/// The shell destroys items on volumes without a Recycle Bin, so only fixed local volumes are accepted. The volume
/// is the one that actually holds the path, which differs from its drive letter under a folder mount point.
/// Returns its NUL-terminated root, which ends in a backslash.
fn recycle_bin_volume(path: &Path) -> Result<Vec<u16>, String> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_FIXED;

    let canonical = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
    if matches!(
        canonical.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    ) {
        return Err("the Recycle Bin is not available for network paths".to_string());
    }
    let wide: Vec<u16> = canonical.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume = vec![0u16; wide.len().max(261)];
    // SAFETY: `wide` is NUL-terminated and `volume` is writable for the length passed; both outlive the call.
    let found =
        unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if found == 0 {
        return Err(format!(
            "could not resolve the volume: {}",
            std::io::Error::last_os_error()
        ));
    }
    let end = volume
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(volume.len());
    volume.truncate(end);
    volume.push(0);
    // SAFETY: GetVolumePathNameW succeeded, so `volume` holds a NUL-terminated root path.
    let drive_type = unsafe { GetDriveTypeW(volume.as_ptr()) };
    if drive_type == DRIVE_FIXED {
        Ok(volume)
    } else {
        Err("the Recycle Bin is only used on fixed local drives".to_string())
    }
}

/// The braced GUID of `\\?\Volume{GUID}\`, the subkey name of the volume's Recycle Bin settings.
fn volume_guid(root: &[u16]) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;

    let mut name = [0u16; 64];
    // SAFETY: `root` is a NUL-terminated volume root ending in a backslash, as the API requires, and `name` is
    // writable for the length passed; both outlive the call.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    };
    if ok == 0 {
        return None;
    }
    let end = name.iter().position(|&unit| unit == 0)?;
    let name = String::from_utf16(&name[..end]).ok()?;
    let start = name.find('{')?;
    let close = name.rfind('}')?;
    (start < close).then(|| name[start..=close].to_string())
}

fn user_total_bytes(root: &[u16]) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `root` is a NUL-terminated directory path and the three outputs are writable u64s; all outlive
    // the call.
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::fs;
    use windows::Win32::UI::Shell::TSF_NORMAL;

    fn wide_to_string(wide: &[u16]) -> String {
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    }

    fn sink() -> (ComObject<RecycleOnlySink>, IFileOperationProgressSink) {
        let object = ComObject::new(RecycleOnlySink::default());
        let interface = object.to_interface();
        (object, interface)
    }

    #[test]
    fn sink_vetoes_any_delete_that_is_not_a_recycle() {
        let (object, interface) = sink();
        let recycle = TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32;
        // SAFETY: an in-process COM object; a null item is allowed because the sink never reads it.
        unsafe {
            assert!(interface.PreDeleteItem(recycle, None).is_ok());
            assert!(interface.PreDeleteItem(recycle | 0x40, None).is_ok());
            assert_eq!(
                interface
                    .PreDeleteItem(TSF_NORMAL.0 as u32, None)
                    .unwrap_err()
                    .code(),
                E_ABORT
            );
        }
        assert_eq!(object.report().vetoed, 1);
    }

    #[test]
    fn sink_flags_a_delete_without_a_recycle_bin_item() {
        let (object, interface) = sink();
        let recycle = TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32;
        // SAFETY: an in-process COM object; null items are allowed because the sink only tests them for null.
        unsafe {
            assert_eq!(
                interface
                    .PostDeleteItem(recycle, None, HRESULT(0), None)
                    .unwrap_err()
                    .code(),
                E_ABORT
            );
            assert!(interface
                .PostDeleteItem(recycle, None, E_ABORT, None)
                .is_ok());
        }
        let report = object.report();
        assert_eq!(report.destroyed, 1);
        assert_eq!(report.failed, Some(E_ABORT));
        assert_eq!(report.recycled, 0);
    }

    #[test]
    fn outcome_succeeds_only_when_every_item_reached_the_recycle_bin() {
        let clean = SinkReport {
            recycled: 2,
            ..SinkReport::default()
        };
        assert_eq!(outcome(Ok(false), clean, 2), Ok(()));
        assert!(outcome(Ok(false), clean, 3).is_err());
        assert!(outcome(Ok(true), clean, 2).is_err());
        assert!(outcome(Err(E_ABORT.into()), clean, 2).is_err());
        for report in [
            SinkReport { vetoed: 1, ..clean },
            SinkReport {
                destroyed: 1,
                ..clean
            },
            SinkReport {
                failed: Some(E_ABORT),
                ..clean
            },
        ] {
            assert!(outcome(Ok(false), report, 2).is_err(), "{report:?}");
        }
        let destroyed = outcome(
            Ok(false),
            SinkReport {
                destroyed: 1,
                vetoed: 1,
                ..clean
            },
            2,
        )
        .unwrap_err();
        assert!(destroyed.contains("without placing them"), "{destroyed}");
    }

    #[test]
    fn verbatim_paths_are_converted_for_the_shell() {
        let name = |path: &str| shell_parsing_name(Path::new(path)).map(|w| wide_to_string(&w));
        assert_eq!(name(r"C:\a\b.jsonl").unwrap(), r"C:\a\b.jsonl");
        assert_eq!(name(r"\\?\C:\a\b").unwrap(), r"C:\a\b");
        assert_eq!(name(r"\\?\UNC\host\share\a").unwrap(), r"\\host\share\a");
        assert_eq!(name(r"\\host\share\a").unwrap(), r"\\host\share\a");
        assert!(name(r"relative\a").is_err());
        assert!(name(r"\\.\COM1").is_err());
    }

    #[test]
    fn recycle_refuses_paths_off_fixed_local_volumes_before_any_shell_call() {
        let dir = TempDir::new("recycle-volume");
        let root = recycle_bin_volume(dir.path()).unwrap();
        assert_eq!(root.last(), Some(&0));
        assert!(volume_guid(&root).is_some_and(|guid| guid.starts_with('{') && guid.ends_with('}')));
        assert!(user_total_bytes(&root).is_some_and(|total| total > 0));
        assert!(recycle_bin_volume(&dir.path().join("absent")).is_err());
        let file = dir.path().join("keep.txt");
        fs::write(&file, "keep").unwrap();
        assert!(recycle(&[file.clone(), dir.path().join("absent")]).is_err());
        assert!(file.is_file());
    }

    #[test]
    fn recycle_refuses_before_any_shell_call_when_the_settings_would_delete() {
        use crate::windows_recycle_settings::tests::FakeRegistry;
        use crate::windows_recycle_settings::Hive;

        let dir = TempDir::new("recycle-settings");
        let sidecar = dir.path().join("session-id");
        fs::create_dir_all(&sidecar).unwrap();
        fs::write(sidecar.join("agent-a.jsonl"), "{}\n").unwrap();
        let transcript = dir.path().join("session-id.jsonl");
        fs::write(&transcript, "{}\n").unwrap();
        let paths = [sidecar.clone(), transcript.clone()];
        let cases: [(&str, &str, Option<u32>, &str); 3] = [
            (
                "Policies",
                "NoRecycleFiles",
                Some(1),
                "NoRecycleFiles policy",
            ),
            (
                r"BitBucket\Volume\{",
                "NukeOnDelete",
                Some(1),
                "on this drive",
            ),
            (
                r"BitBucket\Volume\{",
                "MaxCapacity",
                Some(0),
                "may hold (0 MiB)",
            ),
        ];
        for (key_part, name, answer, expected) in cases {
            let registry = FakeRegistry(|hive: Hive, key: &str, value: &str| {
                Ok(
                    (hive == Hive::CurrentUser && key.contains(key_part) && value == name)
                        .then_some(answer)
                        .flatten(),
                )
            });
            let err = recycle_checked(&paths, &registry).unwrap_err();
            assert!(err.contains(expected), "{name}: {err}");
            assert!(sidecar.join("agent-a.jsonl").is_file() && transcript.is_file());
        }
    }

    /// Live checks against the real shell and Recycle Bin. Only files created here are touched; recycled items are
    /// restored from the Recycle Bin, never purged, and the bin is never emptied.
    mod live {
        use super::*;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn live_dir(label: &str) -> PathBuf {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();
            let dir = std::env::temp_dir().join(format!(
                "bat-recycle-live-{label}-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn populate(dir: &Path) -> (PathBuf, PathBuf) {
            let sidecar = dir.join("session-id");
            fs::create_dir_all(sidecar.join("subagents")).unwrap();
            fs::write(sidecar.join("subagents").join("agent-a.jsonl"), "{}\n").unwrap();
            let transcript = dir.join("session-id.jsonl");
            fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
            (sidecar, transcript)
        }

        /// The shell records the original location of a long path in verbatim (`\\?\`) form.
        fn without_verbatim(path: &Path) -> PathBuf {
            let text = path.to_string_lossy();
            PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
        }

        fn restore_from_recycle_bin(originals: &[PathBuf]) -> usize {
            let items: Vec<trash::TrashItem> = trash::os_limited::list()
                .unwrap()
                .into_iter()
                .filter(|item| originals.contains(&without_verbatim(&item.original_path())))
                .collect();
            let count = items.len();
            if count > 0 {
                trash::os_limited::restore_all(items).unwrap();
            }
            count
        }

        fn admin_share_path(path: &Path, host: &str) -> Option<PathBuf> {
            let text = path.to_str()?;
            let (drive, rest) = text.split_once(":\\")?;
            Some(PathBuf::from(format!(
                r"\\{host}\{}$\{rest}",
                drive.to_ascii_lowercase()
            )))
        }

        #[test]
        #[ignore = "uses the real Recycle Bin; run with --ignored on Windows"]
        fn live_local_items_go_to_the_recycle_bin_and_are_restored() {
            let dir = live_dir("local");
            let (sidecar, transcript) = populate(&dir);
            let paths = vec![sidecar.clone(), transcript.clone()];
            let result = recycle(&paths);
            println!("recycle result: {result:?}");
            println!(
                "after recycle: sidecar exists={} transcript exists={}",
                sidecar.exists(),
                transcript.exists()
            );
            assert_eq!(result, Ok(()));
            assert!(!sidecar.exists() && !transcript.exists());
            let restored = restore_from_recycle_bin(&paths);
            println!("restored from the Recycle Bin: {restored} item(s)");
            println!(
                "after restore: sidecar exists={} subagent exists={} transcript exists={}",
                sidecar.exists(),
                sidecar.join("subagents").join("agent-a.jsonl").is_file(),
                transcript.is_file()
            );
            assert_eq!(restored, 2);
            assert!(transcript.is_file());
            assert!(sidecar.join("subagents").join("agent-a.jsonl").is_file());
            let left_in_bin = items_in_recycle_bin_under(&dir);
            println!("test items still in the Recycle Bin: {left_in_bin}");
            assert_eq!(left_in_bin, 0);
            fs::remove_dir_all(&dir).unwrap();
        }

        /// An item that cannot be moved stops the operation: items queued after it stay, items before it are
        /// already in the Recycle Bin.
        #[test]
        #[ignore = "uses the real Recycle Bin; run with --ignored on Windows"]
        fn live_items_are_processed_in_order_and_stop_at_the_first_failure() {
            use std::os::windows::fs::OpenOptionsExt;
            let dir = live_dir("order");
            let locked = dir.join("locked.jsonl");
            let first = dir.join("first.jsonl");
            let last = dir.join("last.jsonl");
            for path in [&locked, &first, &last] {
                fs::write(path, "{}\n").unwrap();
            }
            let handle = fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&locked)
                .unwrap();

            let blocked_first = recycle(&[locked.clone(), last.clone()]);
            println!("[locked, last] -> {blocked_first:?}");
            println!(
                "after: locked exists={} last exists={}",
                locked.is_file(),
                last.is_file()
            );
            assert!(blocked_first.is_err());
            assert!(locked.is_file() && last.is_file());

            let blocked_second = recycle(&[first.clone(), locked.clone()]);
            println!("[first, locked] -> {blocked_second:?}");
            println!(
                "after: first exists={} locked exists={}",
                first.is_file(),
                locked.is_file()
            );
            drop(handle);
            assert!(blocked_second.is_err());
            assert!(locked.is_file());
            if !first.is_file() {
                let restored = restore_from_recycle_bin(std::slice::from_ref(&first));
                println!("restored from the Recycle Bin: {restored} item(s)");
            }
            assert!(first.is_file());
            assert_eq!(items_in_recycle_bin_under(&dir), 0);
            fs::remove_dir_all(&dir).unwrap();
        }

        /// `trash::os_limited::restore_all` rejects verbatim original locations (E_INVALIDARG), so long paths are
        /// restored through the shell's own undelete verb, limited to items whose origin contains `marker`.
        fn undelete_with_shell(marker: &str) -> String {
            let script = format!(
                "$bin = (New-Object -ComObject Shell.Application).NameSpace(10); \
                 $items = @($bin.Items() | Where-Object {{ \"$($_.ExtendedProperty('System.Recycle.DeletedFrom'))\" -like '*\\{marker}\\*' }}); \
                 foreach ($item in $items) {{ $item.InvokeVerb('undelete') }}; $items.Count"
            );
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        fn items_in_recycle_bin_under(dir: &Path) -> usize {
            trash::os_limited::list()
                .unwrap()
                .iter()
                .filter(|item| without_verbatim(&item.original_parent).starts_with(dir))
                .count()
        }

        /// A path longer than MAX_PATH on a fixed drive passes the volume guard; the shell must either recycle it
        /// or leave it in place.
        #[test]
        #[ignore = "uses the real Recycle Bin; run with --ignored on Windows"]
        fn live_long_path_is_recycled_or_left_in_place() {
            let dir = live_dir("long");
            let mut deep = dir.clone();
            while deep.as_os_str().len() < 300 {
                deep.push("a-fairly-long-folder-name-for-max-path");
            }
            fs::create_dir_all(&deep).unwrap();
            let transcript = deep.join("session-id.jsonl");
            fs::write(&transcript, "{}\n").unwrap();
            println!("path length: {}", transcript.as_os_str().len());
            let result = recycle(std::slice::from_ref(&transcript));
            println!("recycle result: {result:?}");
            println!("after recycle: transcript exists={}", transcript.is_file());
            if result.is_ok() {
                let marker = dir.file_name().unwrap().to_string_lossy().into_owned();
                let restored = undelete_with_shell(&marker);
                println!("restored from the Recycle Bin: {restored}");
                assert_eq!(restored, "1");
            }
            assert!(transcript.is_file());
            assert_eq!(items_in_recycle_bin_under(&dir), 0);
            fs::remove_dir_all(&dir).unwrap();
        }

        /// Network paths are never recycled. The volume guard is bypassed on purpose so the shell itself must
        /// be stopped by the sink; the directory is left in place as evidence.
        #[test]
        #[ignore = "uses the real shell on a loopback share; run with --ignored on Windows"]
        fn live_network_path_is_aborted_instead_of_deleted() {
            let dir = live_dir("unc");
            let (sidecar, transcript) = populate(&dir);
            println!("local evidence directory: {}", dir.display());
            let mut attempted = 0;
            for host in ["localhost", "127.0.0.1"] {
                let remote_sidecar = admin_share_path(&sidecar, host).unwrap();
                let remote_transcript = admin_share_path(&transcript, host).unwrap();
                if !remote_transcript.is_file() {
                    println!(
                        "{host}: {} is not reachable from this account",
                        remote_transcript.display()
                    );
                    continue;
                }
                attempted += 1;
                println!(
                    "{host}: volume guard says {:?}",
                    recycle_bin_volume(&remote_transcript).map(|_| ())
                );
                let names = vec![
                    shell_parsing_name(&remote_sidecar).unwrap(),
                    shell_parsing_name(&remote_transcript).unwrap(),
                ];
                let result = std::thread::spawn(move || recycle_on_sta(&names))
                    .join()
                    .unwrap();
                println!("{host}: shell result {result:?}");
                if result.is_ok() {
                    let restored = restore_from_recycle_bin(&[
                        sidecar.clone(),
                        transcript.clone(),
                        remote_sidecar,
                        remote_transcript,
                    ]);
                    println!("{host}: unexpectedly recycled; restored {restored} item(s)");
                }
                println!(
                    "{host}: after: sidecar exists={} subagent exists={} transcript exists={}",
                    sidecar.is_dir(),
                    sidecar.join("subagents").join("agent-a.jsonl").is_file(),
                    transcript.is_file()
                );
                assert!(result.is_err());
                assert!(sidecar.join("subagents").join("agent-a.jsonl").is_file());
                assert!(transcript.is_file());
            }
            println!("network attempts: {attempted}");
            assert!(attempted > 0, "no loopback share was reachable");
        }

        /// Run only while the caller has set NukeOnDelete=1 for the temp volume; this test never writes the registry.
        #[test]
        #[ignore = "needs NukeOnDelete=1 set externally and BAT_LIVE_NUKE_TEST=1"]
        fn live_nuke_on_delete_volume_is_refused_and_vetoed() {
            if std::env::var("BAT_LIVE_NUKE_TEST").as_deref() != Ok("1") {
                println!("skipped: BAT_LIVE_NUKE_TEST is not 1");
                return;
            }
            let dir = live_dir("nuke");
            let (sidecar, transcript) = populate(&dir);
            let paths = vec![sidecar.clone(), transcript.clone()];
            let still_present = || {
                sidecar.join("subagents").join("agent-a.jsonl").is_file() && transcript.is_file()
            };

            let checked = recycle(&paths);
            println!(
                "pre-check result: {checked:?}; files present={}",
                still_present()
            );

            let names = vec![
                shell_parsing_name(&sidecar).unwrap(),
                shell_parsing_name(&transcript).unwrap(),
            ];
            let shell = std::thread::spawn(move || recycle_on_sta(&names))
                .join()
                .unwrap();
            if shell.is_ok() {
                let restored = restore_from_recycle_bin(&paths);
                println!("shell recycled despite NukeOnDelete; restored {restored} item(s)");
            }
            println!("shell result: {shell:?}; files present={}", still_present());

            assert!(checked.is_err());
            assert!(shell.is_err());
            assert!(still_present());
            fs::remove_dir_all(&dir).unwrap();
        }

        /// Prints this machine's Recycle Bin settings for the temp volume. Only reads the registry.
        #[test]
        #[ignore = "reads this machine's Recycle Bin settings; run with --ignored on Windows"]
        fn live_recycle_bin_settings_are_only_read() {
            use crate::windows_recycle_settings::Hive;

            let dir = std::env::temp_dir();
            let root = recycle_bin_volume(&dir).unwrap();
            let guid = volume_guid(&root);
            let total = user_total_bytes(&root);
            println!(
                "temp volume root={} guid={guid:?} user_total_bytes={total:?}",
                wide_to_string(&root)
            );
            let policies = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
            let bit_bucket = r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket";
            let volume_key = format!(r"{bit_bucket}\Volume\{}", guid.clone().unwrap_or_default());
            let reads = [
                (Hive::CurrentUser, policies, "NoRecycleFiles"),
                (Hive::LocalMachine, policies, "NoRecycleFiles"),
                (Hive::CurrentUser, bit_bucket, "NukeOnDelete"),
                (Hive::LocalMachine, bit_bucket, "NukeOnDelete"),
                (Hive::CurrentUser, volume_key.as_str(), "NukeOnDelete"),
                (Hive::CurrentUser, volume_key.as_str(), "MaxCapacity"),
            ];
            for (hive, key, value) in reads {
                let read = SystemRegistry.dword(hive, key, value);
                println!("{hive:?} {key}\\{value} = {read:?}");
                assert!(read.is_ok(), "{hive:?} {key}\\{value}: {read:?}");
            }
            let check = check_recycle_settings(
                &SystemRegistry,
                &[VolumeLoad {
                    guid,
                    bytes: 1,
                    user_total_bytes: total,
                }],
            );
            println!(
                "verdict for 1 byte: {:?} notes: {:?}",
                check.verdict, check.notes
            );
            assert_eq!(check.verdict, Ok(()));
        }
    }
}
