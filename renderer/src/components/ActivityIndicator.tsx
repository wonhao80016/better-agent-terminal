import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useWorkspace, workspaceStore } from '../stores/workspace-store'
import { useNotifications } from '../stores/notification-store'
import { shallowEqual } from '../stores/use-store'
import { workspaceHasUnreadCompletion } from '../utils/attention'
import {
  ACTIVITY_WINDOW_MS,
  backgroundActivityTooltip,
  resolveActivityDotState,
  summarizeActivity,
  type ActivitySnapshot,
} from '../lib/background-activity'

interface ActivityIndicatorProps {
  lastActivityTime?: number | null
  workspaceId?: string
  terminalId?: string
  size?: 'small' | 'medium'
}

export function ActivityIndicator({
  lastActivityTime: propActivityTime,
  workspaceId,
  terminalId,
  size = 'small'
}: ActivityIndicatorProps) {
  const { t } = useTranslation()
  // Re-renders only when this component's specific slice changes (terminal-scoped
  // or workspace-scoped). Avoids the previous full-store subscription that
  // re-rendered every indicator on any unrelated terminal mutation.
  const activityData = useWorkspace(
    (state): ActivitySnapshot & { hasPending: boolean } => {
      if (terminalId) {
        const terminal = state.terminals.find(t => t.id === terminalId)
        return {
          ...summarizeActivity(terminal ? [terminal] : []),
          hasPending: terminal?.hasPendingAction ?? false,
        }
      }
      if (workspaceId) {
        const terminals = workspaceStore.getWorkspaceTerminals(workspaceId)
        return {
          ...summarizeActivity(terminals),
          hasPending: terminals.some(t => t.hasPendingAction),
        }
      }
      return {
        lastActivityTime: propActivityTime ?? null,
        agentRunning: false,
        backgroundCount: 0,
        backgroundAgentCount: 0,
        backgroundAgentNames: '',
        backgroundCommandNames: '',
        hasPending: false,
      }
    },
    shallowEqual,
  )
  // "Finished, not yet looked at": an unread completion notification for this
  // workspace. Read state flips when the workspace is viewed (NotificationBell
  // owns that), so the dot drops back to the dim inactive green by itself.
  const hasUnread = useNotifications(
    entries => (workspaceId ? workspaceHasUnreadCompletion(entries, workspaceId) : false),
  )
  const [isRecent, setIsRecent] = useState(false)

  // Single timeout for active→inactive transition (replaces 1s interval)
  useEffect(() => {
    const { lastActivityTime } = activityData

    if (!lastActivityTime) {
      setIsRecent(false)
      return
    }

    const timeSinceActivity = Date.now() - lastActivityTime
    if (timeSinceActivity >= ACTIVITY_WINDOW_MS) {
      setIsRecent(false)
      return
    }

    setIsRecent(true)
    const timeout = setTimeout(() => setIsRecent(false), ACTIVITY_WINDOW_MS - timeSinceActivity)
    return () => clearTimeout(timeout)
  }, [activityData.lastActivityTime])

  // Precedence: pending (red) > active (yellow) > background (blue) > unread (bright green) > inactive.
  const stateClass = resolveActivityDotState({
    isActive: isRecent || activityData.agentRunning,
    backgroundCount: activityData.backgroundCount,
    hasUnread,
  })
  const { backgroundCount } = activityData
  const showBackgroundCount = !activityData.hasPending && backgroundCount > 0
  const className = `activity-indicator ${size} ${stateClass}`
    + `${activityData.hasPending ? ' pending' : ''}${showBackgroundCount ? ' has-bg-count' : ''}`
  const title = backgroundActivityTooltip(activityData, t)

  return (
    <div className={className} title={title}>
      {activityData.hasPending && <span className="activity-indicator-badge">?</span>}
      {showBackgroundCount && <span className="activity-indicator-bg-count">{backgroundCount}</span>}
    </div>
  )
}
