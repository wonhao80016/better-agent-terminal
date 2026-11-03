export type SidebarTab = 'workspaces' | 'history'

export const SIDEBAR_TABS: readonly SidebarTab[] = ['workspaces', 'history']
export const SIDEBAR_TAB_STORAGE_KEY = 'bat-sidebar-tab'

type StorageReader = () => Pick<Storage, 'getItem'>
type StorageWriter = () => Pick<Storage, 'setItem'>

const browserStorage = () => localStorage

/** Anything other than a known tab falls back to Workspaces. */
export function parseSidebarTab(value: unknown): SidebarTab {
  return value === 'history' ? 'history' : 'workspaces'
}

export function readStoredSidebarTab(getStorage: StorageReader = browserStorage): SidebarTab {
  try {
    return parseSidebarTab(getStorage().getItem(SIDEBAR_TAB_STORAGE_KEY))
  } catch {
    return 'workspaces'
  }
}

export function writeStoredSidebarTab(tab: SidebarTab, getStorage: StorageWriter = browserStorage): void {
  try {
    getStorage().setItem(SIDEBAR_TAB_STORAGE_KEY, tab)
  } catch {
    // The tab choice is presentation-only; losing it is harmless.
  }
}
