# Git Graph 規劃

Git 頁面的 commit 清單加上分支線圖（git graph），呈現方式參考 VS Code Git Graph：每個 commit 是一個節點，分支、合併以彩色車道線連接，並在訊息前標示 branch 與 tag。

- 狀態：P0–P6 已完成並通過驗證（第 11 節）；第 9 節的決策已由使用者確認（2026-11-11）。
- 基準：本文件的檔案行號以 commit `a89b44e`（`local-dev`）為準。
- 實作分支：`feat/git-graph`。

## 1. 目標與範圍

### 1.1 目標

1. commit 清單左側畫出車道線與節點，正確呈現分支、合併、多個分支匯合。
2. commit 訊息前顯示 ref 標籤：目前分支、本地分支、遠端分支、tag、detached HEAD。
3. 可切換顯示範圍：只看目前分支，或看所有本地分支、遠端分支與 tag。
4. 「未提交變更」列以空心節點加虛線連到 HEAD。
5. graph 模式下可「載入更多」commit，上限 500 筆。

### 1.2 不做

- commit 右鍵操作（checkout、cherry-pick、建立分支等）、搜尋、篩選作者。
- 虛擬捲動（500 筆以內不需要，見 5.7）。
- 改變既有 diff 行為：merge commit 只跟第一個 parent 比、root commit 無法顯示 diff（`hash~1..hash`，`src-tauri/crates/bat-git/src/git.rs:180-190`）維持現狀。

### 1.3 不變式（既有功能不能被影響）

| # | 不變式 | 怎麼保證 |
|---|---|---|
| I1 | 既有 IPC `git_get_log` / `host.git.getLog` / 遠端 `git:log` 的名稱、參數與回傳格式不變 | 只新增指令，不修改舊指令；專案 `AGENTS.md` 的 IPC Compatibility 規定 additive-only |
| I2 | graph 開啟但取得失敗時，Git 頁面顯示與現在完全相同的 commit 清單；使用者關閉 graph 時，清單與現在相同，標題列另有 graph 開關 | 原清單照舊取得並先顯示；graph 只在成功時替換呈現；fallback 模式不套用任何 graph 樣式，也不顯示 graph 開關 |
| I3 | 選取 commit、檔案清單、diff、檔案檢視、變更導覽、worktree 切換、欄寬調整行為不變 | 只改 commit 清單列的呈現；選取仍呼叫既有 `handleSelectCommit(hash)`；載入更多與切換範圍走獨立路徑（5.6），不清空選取、不顯示 Loading |
| I4 | 預設顯示的 commit 集合與現在相同（目前分支，預設 50 筆）；commit 時間沒有逆序時，順序也相同 | scope 預設 `current`；初始筆數沿用 50；切換 worktree 時筆數重設為 50 |
| I5 | graph 失敗不可讓面板誤顯示「不是 Git 儲存庫」 | graph 不放進既有 `Promise.all`（`renderer/src/components/GitPanel.tsx:240-252`），自行處理失敗 |
| I6 | 其他使用 `git.*` i18n key 的元件不受影響 | 只新增 key，不改既有 key（`git.justNow` 等也被 `renderer/src/components/GitHubPanel.tsx:68-73` 使用） |
| I7 | graph 失敗或很慢，開啟 Git 分頁的等待時間最多增加 300 ms | 初次顯示最多只等 graph 300 ms，超過就先顯示原清單、graph 到了再替換；逾時不阻塞（5.2）；同一路徑失敗後本工作階段不再重試（5.3） |

## 2. 現況與接觸點

| 層 | 位置 | 說明 |
|---|---|---|
| Renderer UI | `renderer/src/components/GitPanel.tsx:232-258` | `loadData` 先清空選取，再以 `Promise.all` 取 log、status、branch、root；任一失敗就顯示「不是 Git 儲存庫」；由 `useEffect([loadData])` 觸發，deps 為 `[activePath]` |
| Renderer UI | `renderer/src/components/GitPanel.tsx:342-344` | `loading` 為真時整個面板換成 Loading |
| Renderer UI | `renderer/src/components/GitPanel.tsx:367-405` | commit 清單：未提交變更列（含橘色 `●`）與 commit 列（message、hash、作者、日期） |
| Renderer API | `renderer/src/host-api.ts:944-955` | git 方法註解（回傳安全預設值）與 `host.git.getLog(cwd, count?)` → `git_get_log` |
| 樣式 | `renderer/src/styles/file-browser.css:360-415` | `.git-commit-list`、`.git-commit-item`（padding 6px 8px、border-bottom 1px、active 時 `border-left: 2px`） |
| i18n | `renderer/src/locales/{en,ja,zh-CN,zh-TW}.json` 的 `git` 區段 | en.json:569-589、ja.json:550-570、zh-CN.json:541-561、zh-TW.json:566-586 |
| Tauri 指令 | `src-tauri/src/commands/git.rs:20-23、150-184` | `pub use` 清單；`git_get_log`（遠端視窗走 `remote_invoke_for_window`，錯誤一律變空陣列）與 `git_get_log_native` |
| 指令註冊 | `src-tauri/src/lib.rs:340` | `generate_handler!` 中的 `git_cmd::git_get_log` |
| 解析 | `src-tauri/crates/bat-git/src/git.rs:99-120、213-216` | `parse_log`、`clamp_log_count`（1..500，預設 50） |
| 執行 git | `src-tauri/crates/bat-git/src/git.rs:29-71` | `run_git`：輪詢等程式結束後才讀 stdout |
| 遠端參數表 | `src-tauri/crates/bat-remote-protocol/src/remote_core.rs:517` | `legacy_v1_args_to_params` 的位置參數 → 具名參數對照；client 只送位置參數，host 必須靠這張表取得具名參數 |
| 遠端派送 | `src-tauri/src/remote_server.rs:3696-3700` | host 端處理 `git:log`；headless bat-server 透過 lib 共用同一檔案 |
| 測試 | `tests/host-api.test.ts:130、447-450、863-864` | mock、呼叫與有序呼叫清單含 `git_get_log` |

其他確認過的事實：

- Tauri capabilities（`src-tauri/capabilities/default.json`）只列 plugin 權限，新增 app 指令不需登記；沒有測試比對 `generate_handler!` 清單。
- `git:` 前綴的遠端 channel 會自動走 profile context，沒有逐 channel 的權限控管。
- Node sidecar 與已刪除的 Electron 程式碼都沒有 git log 實作。
- 舊版 host 收到不認得的遠端指令時會轉給 Node sidecar，回 `-32601 method not found`。
- Windows 上 PATH 的 `git` 是 `C:\Program Files\Git\cmd\git.exe` wrapper，會再啟動真正的 git；kill wrapper 不會結束真正的 git。

## 3. 實測資料

本機環境：Windows 10、git 2.56；repo 共 1,625 個 commit、407 個 tag、已有 commit-graph 檔。

| 項目 | 結果 |
|---|---|
| 現有 `git log -n 50`（PowerShell 直接呼叫，3 次） | 1,145–1,231 ms |
| graph 版 log，目前分支 50 筆 | 1,089–1,286 ms |
| graph 版 log，全部分支 500 筆 | 1,001–1,701 ms |
| graph 版輸出大小 | 50 筆 9.7 KB、500 筆 93 KB、1,000 筆 177 KB |
| 用 `run_git` 的讀法跑 500 / 1,000 筆 | 卡住，5 秒逾時，回 `None` |
| 改為「邊讀邊等」跑 500 / 1,000 筆 | 0.49 秒成功 |
| 車道數（原型演算法） | 最多 7 條（PR 合併區段），500 筆平均 1.7 條 |
| `--date-order` 下 parent 排在 child 之前的次數 | 0 |

這台機器啟動 git 行程本身約需 1 秒，兩種 log 的差距在誤差內；graph 多取的欄位不是成本來源。本 repo 的「全部分支」與「目前分支」內容相同（範圍外 commit 數為 0），多分支情境必須用合成資料驗證（第 6 節）。

## 4. 設計總覽

```
GitPanel.loadData ─┬─ Promise.all(getLog, getStatus, getBranch, getRoot)   ← 既有，原樣
                   └─ getGraphLog(cwd, count, scope)（並行、自行處理失敗）
                        ├─ 300 ms 內完成：直接以 graph 呈現
                        └─ 較慢：先顯示原清單，graph 到了再替換；失敗則維持原清單

git_get_graph_log (Tauri) ─┬─ 遠端視窗：remote_invoke("git:graph-log")
                           └─ 本機：git_get_graph_log_native
                                 └─ bat-git: build_graph_log_args → run_git → parse_graph_log

computeGitGraph(rows) ──► 每列的節點、車道線 ──► GitGraphCell（SVG）+ ref 標籤
```

## 5. 詳細設計

### 5.1 資料來源

git 指令（參數順序有意義）：

```
git log --date-order --decorate=full --no-show-signature
        --pretty=format:%H%x1f%P%x1f%D%x1f%an%x1f%aI%x1f%s
        -n <count>
        --ignore-missing [scope=all: --branches --remotes --tags] HEAD
        --
```

- `--date-order`：保證 child 一定在 parent 之前，車道演算法依賴這點。commit 時間沒有逆序時，順序與現有 `git log` 相同。
- 範圍不用 `--all`：它會把 `refs/stash`、`refs/notes`、`refs/prefetch` 等也走進來，產生無意義的節點。
- `--ignore-missing` 必須放在所有 revision 之前才會生效。兩種範圍都加上它：HEAD 尚無 commit（新 repo、orphan branch）時輸出為空且 exit 0，而不是 exit 128；否則這個失敗會被記住，第一次 commit 後仍看不到 graph。一般 repo 的輸出與不加時逐位元組相同（實測）。
- 結尾的 `--` 避免工作目錄裡有名為 `HEAD` 的檔案時出現 ambiguous argument。
- `--no-show-signature`：使用者設定 `log.showSignature=true` 時，gpg 訊息不會混進 stdout。
- 欄位分隔用 `\x1f`，message 放最後並以 `splitn(6, '\x1f')` 解析。欄位數不足 6、或 hash 不是 40／64 位十六進位的行一律略過。
- `%P` 以空白分隔多個 parent；root commit 與 shallow clone 邊界 commit 為空字串，兩者都畫成沒有 parent 的節點。
- `%D` 搭配 `--decorate=full` 輸出完整 ref 名稱，依下表分類（ref 名稱不能含空白，以 `", "` 切分是安全的）：

| `%D` 項目 | 種類 `kind` | 顯示名稱 |
|---|---|---|
| `HEAD -> refs/heads/X` | `branch`，`current: true` | `X` |
| `HEAD`（detached） | `head` | `HEAD` |
| `refs/heads/X` | `branch` | `X` |
| `refs/remotes/X` | `remote` | `X`（例如 `origin/main`、`origin/HEAD`） |
| `tag: refs/tags/X` | `tag` | `X` |
| 其他 `refs/...` | `other` | 去掉 `refs/` 前綴 |
| `grafted`、`replaced` 等不以 `refs/` 開頭的記號 | 略過 | — |

回傳型別（Rust，序列化給 renderer）：

```rust
pub struct GitGraphRef { pub kind: String, pub name: String, pub current: bool }
pub struct GitGraphLogEntry {
    pub hash: String,
    pub parents: Vec<String>,
    pub refs: Vec<GitGraphRef>,
    pub author: String,
    pub date: String,
    pub message: String,
}
```

### 5.2 讀取 git 輸出

原本的 `run_git`（`src-tauri/crates/bat-git/src/git.rs:44-63`）先輪詢 `try_wait` 等程式結束，才讀 stdout。輸出超過 pipe 緩衝區（Windows 實測約在 38 KB 與 93 KB 之間）時，git 寫不出去、不會結束，最後逾時回 `None`。這會讓 Git 頁面所有使用它的指令在大輸出時失敗，例如超過約 64 KB 的 diff 顯示為空白。

依 D2，`run_git` 本身改為邊讀邊等，回傳約定不變（任何失敗都回 `None`）。Git 頁面與 GitHub 面板的 8 個指令（GitHub URL、branch、log、graph log、diff、變更檔案清單、root、status）都經由 `commands/git.rs` 的 `run_git_blocking` 使用它：

- stdout 由另一個執行緒持續讀取，上限沿用 `MAX_OUTPUT_BYTES`（5 MiB）。
- 主執行緒照舊輪詢 `try_wait` 做逾時判斷。
- 逾時：kill 直接子行程後**立即回傳 `None`，不 join 讀取執行緒**。在 Windows 上，wrapper 啟動的真正 git 會繼續跑完並持有 pipe，join 會讓呼叫端一直等下去。殘留的 git 在自己結束後釋放 pipe，讀取執行緒隨之結束。
- 正常結束：join 讀取執行緒；exit code 非 0、讀取失敗或超過上限都回 `None`。

修正後，超過門檻的 diff 會實際顯示出來；接近 5 MiB 上限的 diff 可能讓 diff 畫面變慢。worktree 功能另有一個私有的同名函式（`src-tauri/crates/bat-git/src/worktree.rs:194`），寫法相同，不在 D2 範圍內，未修改。

### 5.3 IPC 與遠端

| 項目 | 內容 |
|---|---|
| Tauri 指令 | `git_get_graph_log(cwd, count?, scope?) -> Option<Vec<GitGraphLogEntry>>`；失敗回 `None`（renderer 收到 `null`），與本模組「回傳安全預設值、不傳 stderr」的慣例一致，同時能和「空清單」區分 |
| native 函式 | `git_get_graph_log_native(cwd, count, scope)`，不受 `desktop` feature 限制，供遠端 host 與 headless server 共用 |
| 遠端 channel | `git:graph-log`；參數表加 `"git:graph-log" => Some(&["cwd", "count", "scope"])`；`remote_server.rs` 新增派送分支；遠端呼叫失敗時 Tauri 指令回 `None` |
| Renderer API | `host.git.getGraphLog(cwd, count?, scope?)`，回傳陣列或 `null` |
| scope 值 | `current`（預設，與現行 `git log` 同一組 commit）或 `all`；其他值視為 `current` |
| count | 沿用 `clamp_log_count`（1..500，預設 50） |

renderer 的取得規則（`fetchGraph`，不會 reject）：

1. 若 `path + scope` 已記錄為失敗，直接視為失敗，不呼叫。
2. 呼叫 `getGraphLog`；回 `null` 或拋錯即為失敗，記錄到模組層級的失敗集合，並以 `host.debug.log`（加 `.catch`）記一筆，每個 `path + scope` 只記一次。
3. scope 為 `all` 且失敗時，改用 `current` 再試一次；成功就以 `current` 呈現，所以不會卡在沒有切換鈕的狀態。
4. 都失敗時回傳 `null`，畫面維持原清單（fallback 模式）。
5. 手動按重新整理時清除該 path 的失敗紀錄；手動切換範圍時清除該 path 在目標範圍的失敗紀錄，讓使用者的操作一定會重試。debug log 的「只記一次」另以獨立集合控制，不受清除影響；記錄函式拋錯時也不影響 `load` 的結果。
6. 載入更多使用 `retryCurrent: false, rememberFailure: false`：失敗時維持目前畫面，不改用其他範圍，也不把「在目前筆數下可用」的範圍記為失敗，下次點擊會再試。

舊版 host 在每個 path 的每個範圍、每個工作階段最多自動收到一次 `git:graph-log`（記住的範圍是「全部分支」時，`all` 與改用的 `current` 各一次）；使用者手動重新整理或切換範圍時會再試一次（第 5 點）。不另外做能力旗標。

### 5.4 車道演算法

位置：`renderer/src/lib/git-graph.ts`，純函式 `computeGitGraph(rows)`，不依賴 React。

輸入是依顯示順序排列的 rows（`{ hash, parents, virtual? }`）。有未提交變更時，最前面插入一個虛擬列，其 parent 為 HEAD commit。HEAD 的辨識方式：refs 中有 `current: true` 或 `kind: 'head'` 的 commit；scope 為 `current` 時也可視為第一列。找不到 HEAD（unborn branch、或 HEAD 不在已載入範圍）時，虛擬列沒有 parent，只畫一個獨立節點。

狀態是 `lanes`：每列開始時是一個沒有空洞的 track 陣列（由左到右），每個 track 記錄 `{ hash: 期待的 commit, color, virtual }`。處理第 i 列：

1. **找 match**：`matches` 是期待此 commit 的 track 索引（列開始時的索引）；`realMatches` 是其中非虛擬的部分。
2. **決定節點**：節點欄 = `realMatches` 的最左一個；沒有的話取 `matches` 的最左一個；都沒有就是新的分支頭，放在最右側新欄（索引 = 目前 track 數）。節點顏色 = 最左非虛擬 match 的顏色；沒有非虛擬 match 時配新色（虛擬列本身固定使用中性色）。
3. **入線**：每個 match 畫一條「上緣（match 的欄）→ 節點」的線，顏色與虛線屬性取自該 track；這些 track 在此結束。
4. **第一個 parent**：放回節點所在的欄，顏色為節點顏色；`virtual` 只在本列是虛擬列時為真（從真實 commit 延續的線一律實線）。
5. **其他 parent**：若已有**非虛擬** track 期待它，連到其中最左的一條；否則放進第一個空欄（沒有空欄就加在最右），配新色。merge 不會接到未提交變更的虛線，即使兩者都通往 HEAD。
6. **直通線**：沒有參與本列的 track 畫「上緣 → 下緣」。
7. **列底壓縮**：移除空欄。所有「→ 下緣」的線，終點欄都是**壓縮後**的索引；所有「上緣 →」的線，起點欄都是列開始時的索引。換欄以 S 曲線呈現。

規則與邊界：

- 同一個 commit 可以同時被多條 track 期待（平行線），在該 commit 那一列匯合，與 VS Code Git Graph 相同。
- 顏色：全域遞增計數器對 8 色調色盤取餘數；track 開始時配定，換欄後保持。
- parent 不在已載入範圍內（超出筆數）時，track 一直延伸到清單底部。
- 沒有 parent 的 commit（root、shallow 邊界、孤立 commit），節點所在欄在列底釋放。
- graph 寬度（欄數）= 所有列中「節點欄、線段起點與終點欄」的最大索引 + 1。
- 複雜度 O(列數 × 車道數)，只在 commits 改變時以 `useMemo` 重算。

輸出：每列的 `{ lane, color, virtual, lines: [{ fromLane, toLane, from: 'top' | 'node', to: 'node' | 'bottom', color, dashed }] }`，以及 graph 欄數。

### 5.5 繪製

- graph 模式時，commit 清單容器設定 CSS 變數 `--git-graph-width`（欄數 × 12px）。
- `.git-commit-item.git-commit-item-graph`：`position: relative`；`padding-left: calc(min(var(--git-graph-width), 40%) + 4px)`。graph 模式不用左右 border，所以 padding 的百分比與 cell 寬度的百分比都以清單內容寬為基準，兩者對齊。
- `GitGraphCell`：`position: absolute; left: 0; top: 0; bottom: -1px; width: min(var(--git-graph-width), 40%); overflow: hidden`。往下延伸 1px 蓋住列分隔線，讓線條在列與列之間連續；超出 40% 的欄位被裁切，該處的節點不顯示。
- SVG：寬度固定為 `欄數 × 12` px（不用 100%，避免水平壓縮），`height="100%"`，`viewBox="0 0 W 100"`，`preserveAspectRatio="none"`，讓垂直方向隨列高伸縮（列高會因 meta 換行而不同）。線條加 `vector-effect="non-scaling-stroke"`，粗細維持 2px。
- 座標：`x = lane × 12 + 6`；上緣 y=0、節點 y=50、下緣 y=100。換欄用兩端垂直切線的三次貝茲曲線（`C x1 ym, x2 ym, x2 y2`），垂直方向縮放後切線仍垂直，跨列接縫平滑。
- 節點用 HTML 圓點（直徑 8px）定位在 `left: x - 4px; top: 50%`，不受 SVG 拉伸影響。一般 commit 為實心；未提交變更為空心，外框使用既有的橘色 `#f59e0b`，連到 HEAD 的線為灰色虛線。
- graph 模式下，選取列改用 `border-left: none; box-shadow: inset 2px 0 0 <accent>` 呈現原本的左側色條，避免 border 讓 graph 往右偏 2px。
- 所有 graph 專用樣式都掛在 `git-commit-item-graph` 等 graph 專用 class 下；fallback 模式完全維持原樣式。
- 調色盤 8 色，深色與淺色主題都可讀；虛擬列用 `var(--text-secondary)`。

### 5.6 UI 行為

| 項目 | 行為 |
|---|---|
| 初次載入 | 沿用 `loadData`；graph 與既有 `Promise.all` 並行。`Promise.all` 完成後最多再等 graph 300 ms：已完成就直接以 graph 呈現；否則先以原清單呈現並結束 Loading，graph 到了再替換，並套用與獨立路徑相同的選取規則。300 ms 是因為 Windows 上同時啟動 5 個 git 行程時完成時間會互差數百毫秒，等太短會讓清單先出現再被替換而跳動 |
| 範圍切換 | 標題列、重新整理按鈕左側的小按鈕，在「目前分支 / 全部分支」間切換；只在 graph 模式顯示。走獨立路徑（見下）。成功取得才寫入 `localStorage` 的 `better-terminal-git-graph-scope`；預設 `current`。屬於 client 端的純呈現狀態，遠端模式下也不送 host |
| 載入更多 | 只在 graph 模式、且清單筆數等於目前上限時出現；筆數 50 起，每次 +100，最多 500。走獨立路徑；失敗時維持目前畫面（5.3 第 6 點） |
| 獨立路徑（範圍切換、載入更多） | 不設 `loading`、不清空選取；用 request id 丟棄過期回應；選取的 commit 不在新清單時才清空選取與右側內容（`working` 不受影響）；失敗時維持目前畫面 |
| 過期回應 | `loadData` 與獨立路徑共用遞增 request id；切換 worktree 或重新整理後，舊請求的結果一律丟棄 |
| 筆數重設 | 切換 worktree（`activePath` 改變）時重設為 50；重新整理與範圍切換保留目前筆數 |
| ref 標籤 | 顯示在 message 前；目前分支粗體；顏色取該列節點顏色；排序依序為 detached HEAD、目前分支、本地分支、遠端分支、tag、其他；最多顯示 3 個，其餘以 `+N` 表示，滑鼠停留在 `+N` 上時列出其餘的標籤 |
| 未提交變更 | 沿用現有列與點擊行為；graph 模式以空心橘色節點取代文字前的 `●`，虛線連到 HEAD commit；fallback 模式保留 `●` |
| graph 開關 | 標題列的分支圖示按鈕，開啟時以強調色顯示；存在 `localStorage` 的 `better-terminal-git-graph-enabled`，預設開啟。關閉時：清除 graph 與進行中的 graph 請求、回到原本的清單樣式，之後的載入**不再取 graph**；選取的 commit 不在原清單中才清除。重新開啟時清除該 path 的失敗紀錄並重新取得。開關在 graph 可用、使用者已關閉、或切換進行中時顯示；graph 開啟但取不到時隱藏，以符合 I2 |
| Fallback 模式 | 與現在完全相同：無 graph、無範圍切換、無載入更多、無 graph 開關 |

新增 i18n key（四個語系）：`git.scopeCurrent`、`git.scopeAll`、`git.scopeToggleTitle`、`git.loadMore`。

### 5.7 效能與資源

| 項目 | 成本 |
|---|---|
| git 執行 | graph 開啟時，每次開啟或重新整理多一個 git 行程（graph），與既有 log 並行；耗時與現有 log 同等級（第 3 節）；graph 關閉時沒有額外行程；沒有輪詢 |
| 傳輸 | 500 筆約 93 KB；遠端回應上限 4 MiB |
| 車道計算 | 500 筆 × 最多數條車道，毫秒等級，只在資料變動時算一次 |
| 繪製 | 每列一個小 SVG（通常 1–8 條線）；500 列約數千個 SVG 元素，WebView 可輕鬆處理 |
| 記憶體 | 每筆多存 parent hash 與 ref 名稱，可忽略 |
| 失敗成本 | 同一 path + scope 每個工作階段最多一次逾時；逾時殘留的 git 跑完即結束 |

## 6. 測試計畫

| 層 | 內容 |
|---|---|
| Rust 單元測試（bat-git） | `parse_graph_log`：多 parent、root、ref 分類（`HEAD ->`、detached HEAD、tag、遠端、`origin/HEAD`、其他、`grafted` 略過）、訊息含分隔字元、略過 gpg 等非紀錄行；`build_graph_log_args`：兩種 scope、筆數上限、`--ignore-missing` 在 `HEAD` 之前、`--` 在最後；`run_git`：臨時 repo 中讀出超過 100 KB 的 commit 訊息與超過 100 KB 的 diff（修正前的實作會逾時失敗，已實測） |
| Rust 單元測試（remote） | `legacy_v1_args_to_params("git:graph-log", [cwd, count, scope])` 產生正確的具名參數 |
| Renderer 單元測試 | `tests/git-graph.test.ts` 以合成資料測：線性歷史、分支與合併、多分支頭、多分支平行匯合、octopus merge、merge 的第二個 parent 已在 track 中、parent 未載入、root 與孤立 commit（寬度含其節點）、虛擬列（HEAD 在第一列、在中間、與實線同時 match、merge 以 HEAD 為第二個 parent、找不到 HEAD）、換欄壓縮、顏色延續；另測 ref 排序與標籤截斷。所有案例與 3,000 個固定種子的隨機歷史都檢查結構不變式：列與列之間每個欄位連續且樣式一致、有子 commit 才有入線、真實 commit 不使用虛擬色、欄位不超出寬度 |
| 既有測試 | `tests/host-api.test.ts` 加入 `getGraphLog` 的 mock 與呼叫順序；其他既有測試全部維持通過 |
| 建置 | `pnpm exec tsc --noEmit --pretty false`、`pnpm run compile`、`pnpm run check:tauri-rust`、`pnpm run test:tauri-rust`、headless：`cargo check --manifest-path src-tauri/Cargo.toml --bin bat-server --no-default-features --features headless` |
| 視覺 | 以本 repo 真實資料與合成多分支資料靜態渲染 graph 列，用 Edge headless 截圖檢查線條連續、換欄曲線、節點、標籤、窄欄裁切 |
| 獨立驗收 | fresh-context 驗收 agent 與審查 agent 檢查 diff 與不變式 |

無法在本機驗證的部分：遠端模式端對端、macOS／Linux 的 WebView 呈現、在執行中的 app 實際操作（使用者正在使用 BAT，app 為單一實例）、沒有 commit-graph 的超大 repo 實際耗時。

## 7. 實作階段與 commit

全部在 `feat/git-graph` 分支，每個階段完成並通過該階段驗證後 commit 一次。

| 階段 | 內容 | 驗證 |
|---|---|---|
| P0 | 本規劃文件與審查紀錄 | 文件審查 |
| P1 後端 | bat-git：型別、`build_graph_log_args`、`parse_graph_log`、`run_git_capture` 與測試（P5 併入 `run_git`）；`commands/git.rs`：`git_get_graph_log`、native、`pub use`；`lib.rs` 註冊；遠端參數表與其測試、派送分支 | `check:tauri-rust`、headless check、bat-git、remote-protocol 與主 crate 測試 |
| P2 資料與演算法 | `host.git.getGraphLog` 與 git 方法註解；`tests/host-api.test.ts`；`renderer/src/lib/git-graph.ts`；`tests/git-graph.test.ts`；`package.json` 新增 `test:git-graph` 並串進 `verify:tauri-pre-ci` | tsc、`test:git-graph`、host-api 測試 |
| P3 UI | `GitGraph.tsx`（cell 與 ref 標籤）、`GitPanel.tsx` 整合（漸進載入、失敗處理、範圍切換、載入更多、未提交節點）、CSS、四語系 i18n | tsc、compile、視覺截圖 |
| P4 收尾 | 全面驗證、獨立驗收、修正、更新本文件狀態 | 第 6 節全部項目 |
| P5（D2） | `run_git` 改為邊讀邊等，`run_git_capture` 併入；大輸出測試改測 `run_git` 並加入大 diff | 修正前後各跑一次該測試；`cargo check`、headless check、workspace 測試 |
| P6（D3） | graph 開關、設定值解析與測試、i18n | tsc、compile、`test:git-graph`、標題列截圖 |

## 8. 風險

| # | 風險 | 對策 |
|---|---|---|
| R1 | 非常大的 repo 沒有 commit-graph 檔時，`--date-order` 可能要走訪大量歷史而逾時 | 原清單照常先顯示（I7）；同一路徑本工作階段不再重試 |
| R2 | 遠端連到舊版 host | 新 channel 失敗即維持原清單；每個 path 每工作階段最多送一次 |
| R3 | commit 欄很窄時 graph 佔太多空間 | graph 寬度上限為列寬 40%，超出裁切，被裁掉欄位的節點不顯示 |
| R4 | macOS WKWebView、Linux WebKitGTK 的呈現差異 | 只用標準 SVG 屬性與 CSS `min()`；無實機可驗，列為限制 |
| R5 | graph 模式下選取列的左側色條改用 box-shadow | 視覺相同、不位移；fallback 模式不受影響 |
| R6 | 逾時時 Windows 上殘留真正的 git 行程 | 它會自己跑完並釋放 pipe；R1 的失敗紀錄讓它每個 path 每工作階段最多出現一次 |

## 9. 使用者決策

以下項目由使用者於 2026-11-11 決定：

| # | 決策 | 結果 |
|---|---|---|
| D1 | 預設顯示範圍：目前分支，或全部分支（附圖的樣子） | 目前分支（維持現有清單內容）；可切換並記住 |
| D2 | 是否修正既有 `run_git` 大輸出卡死（超過約 64 KB 的 diff 會變成空白） | 修正，在本分支以獨立 commit 進行（5.2、P5） |
| D3 | 是否需要完全關閉 graph 的開關 | 加入標題列開關，關閉時回到原本的清單樣式並記住選擇（5.6、P6） |
| D5 | 「全部分支」是否包含遠端分支與 tag | 包含，與附圖一致 |

已自行決定的實作參數：「載入更多」初始 50、每次 +100、最多 500；初次顯示等待 graph 的上限 300 ms（理由見 5.6）。

## 10. 審查紀錄

P0 由 fresh-context 審查 agent 對照程式碼審查（2026-11-10）。結論為「修正後可實作」，沒有阻擋項。文件引用的行號與描述經核對全部吻合。發現與處理如下：

| 嚴重度 | 發現 | 處理 |
|---|---|---|
| 重要 | `--ignore-missing` 放在 `HEAD` 之後不生效（實測 exit 128） | 移到所有 revision 之前，並以測試檢查順序（5.1、第 6 節） |
| 重要 | fallback 隱藏範圍切換，scope 已持久化時會卡在 fallback | `all` 失敗改用 `current` 重試；成功才寫入 scope（5.3、5.6） |
| 重要 | 載入更多／切換範圍若沿用 `loadData` 會清空選取並卸載面板 | 定義獨立路徑、request id、筆數重設與選取規則（5.6、I3） |
| 重要 | 失敗時多等一次逾時；Windows kill wrapper 後真正的 git 仍存活，join 會卡住 | 改為漸進載入、不 join 讀取執行緒、失敗紀錄（I7、5.2、5.3） |
| 重要 | 虛擬列與實線同時期待 HEAD 時，節點顏色與虛線屬性未定義 | 決定節點時忽略虛擬 track；延續線一律實線（5.4 步驟 2、4） |
| 重要 | `preserveAspectRatio="none"` 配寬度上限時，SVG 寬度未定義會被水平壓縮 | SVG 固定 px 寬、由 cell 裁切；item 設 `position: relative`（5.5） |
| 建議 | `log.showSignature=true` 時 gpg 訊息混入 stdout | 加 `--no-show-signature`，略過格式不符的行（5.1） |
| 建議 | revision 後應加 `--` | 已加（5.1） |
| 建議 | 演算法有數處不足以唯一決定實作 | 補上座標基準、最左 track、寬度定義、配色、HEAD 辨識（5.4） |
| 建議 | shallow 邊界敘述不準；`%D` 有 `grafted`、`replaced` 記號 | 修正敘述並略過非 ref 記號（5.1、5.4） |
| 建議 | I4 措辭過強 | 改為「commit 時間沒有逆序時順序相同」（I4） |
| 建議 | 本 repo 測不到多分支情境；大輸出測試、遠端參數表測試、headless 建置未列 | 補進第 6 節 |
| 建議 | host-api 註解、debug log 頻率、未提交列的 `●`、能力旗標 | 改為回傳 `null` 以符合註解；每 path 只記一次；graph 模式以空心節點取代 `●`；不做能力旗標，以失敗紀錄限制舊 host 呼叫次數（5.3、5.6） |

P4 由 fresh-context 驗收 agent（逐條驗收條件）與審查 agent（diff 正確性、安全性，含 4 個種子 × 20,000 次隨機歷史的演算法模糊測試）並行檢查 P1–P3 的成果（2026-11-10）。驗收 A1–A7 全部通過，審查結論為「修正後可合併」，沒有阻擋項；審查確認 Rust 端沒有死鎖、執行緒或 handle 洩漏，也沒有參數注入。發現與處理如下：

| 嚴重度 | 發現 | 處理 |
|---|---|---|
| 重要 | merge 的第二個 parent 是 HEAD、且有未提交變更時，merge 線接到虛線，畫成灰色（實測重現） | 合併時略過虛擬 track（5.4 步驟 5）；新增回歸測試與結構不變式檢查，並確認修正前的演算法會被該檢查擋下 |
| 輕微 | 「載入更多」失敗一次後被記為失敗，之後點擊無效，重開分頁也沒有 graph | 載入更多不改用其他範圍、不記錄失敗（5.3 第 6 點） |
| 輕微 | 新 repo（HEAD 尚無 commit）時目前分支的 log exit 128 並被記住 | 兩種範圍都加 `--ignore-missing ... HEAD`（5.1） |
| 輕微 | graph 晚於原清單到達時，沒有清除已不在清單中的選取 | 晚到時套用相同的選取規則（5.6） |
| 輕微 | 程式等待 300 ms，規格寫 150 ms | 採用 300 ms 並寫明理由（I7、5.6） |
| 輕微 | 測試缺少列與列之間的連續性檢查；Rust 大輸出測試在沒有 git 時靜默通過 | 新增結構不變式與隨機歷史測試；略過時印出訊息 |
| 輕微 | 兩段文件註解連同分隔符超過 3 行；記錄函式拋錯會讓 `load` reject | 改為 2 行註解；記錄函式的例外在內部攔截 |
| 輕微 | 舊 host 呼叫次數與 `+N` 提示的文件措辭和實作不符 | 修正 5.3、5.6 的措辭 |

## 11. 實作與驗證結果

### 11.1 實作內容

commit 以 `feat/git-graph` 分支為準，依階段為：`dcfa7be`（P0 規劃）、`9ac1cee`（P1 後端）、`3e83a1d`（P2 資料與演算法）、`2379d3e`（P3 UI）、`5ab31e9`／`1af942d`／`54f9282`（P4 修正與文件）、`f68418d`（P5，D2：`run_git` 大輸出）、`75fb78c`（P6，D3：graph 開關），之後是本文件的決策與結果更新。

| 層 | 檔案 |
|---|---|
| Rust | `src-tauri/crates/bat-git/src/git.rs`（型別、`run_git`、`build_graph_log_args`、`parse_graph_log` 與測試）、`src-tauri/src/commands/git.rs`、`src-tauri/src/lib.rs`、`src-tauri/src/remote_server.rs`、`src-tauri/crates/bat-remote-protocol/src/remote_core.rs` |
| Renderer 邏輯 | `renderer/src/lib/git-graph.ts`、`renderer/src/lib/git-graph-source.ts`、`renderer/src/host-api.ts` |
| Renderer UI | `renderer/src/components/GitGraph.tsx`、`renderer/src/components/GitPanel.tsx`、`renderer/src/styles/file-browser.css`、`renderer/src/locales/{en,ja,zh-CN,zh-TW}.json` |
| 測試與腳本 | `tests/git-graph.test.ts`、`tests/host-api.test.ts`、`package.json`（`test:git-graph`，已串進 `verify:tauri-pre-ci`） |

### 11.2 驗證證據

| 項目 | 結果 |
|---|---|
| `pnpm exec tsc --noEmit --pretty false`、`pnpm run compile` | exit 0 |
| `cargo check --workspace`；headless `bat-server` check | exit 0；本分支新增的程式碼沒有 warning |
| `cargo test --workspace --lib`（Git Bash，長路徑 TEMP） | P4 修正後 744 個全部通過（主 crate 345 個）。P5 後為 743 個（併入後少一個重複測試）：一次與 `pnpm run compile` 同時執行時，`bat-agent-bridge` 有 3 個端對端測試因 sidecar 逾時失敗；該 crate 不依賴 `bat-git`，單獨重跑兩次都是 31 個全部通過 |
| `run_git` 大輸出（P5） | 新測試讀出超過 100 KB 的 commit 訊息與 diff；修正前的實作兩次讀取都卡到 20 秒逾時而失敗，修正後通過 |
| `test:git-graph`（含 3,000 個隨機歷史與設定值解析）、`test:host-api`、`test:host-direct-calls`、`test:renderer-logging`、`test:unified-diff`、`test:workspace-restore`、`test:session-history` | 全部 exit 0 |
| 畫面 | 以元件與 CSS 靜態渲染本 repo 真實資料與合成多分支資料，用 Edge headless 截圖檢查：線條連續、換欄曲線、匯合、octopus、孤立 root、未提交虛線、標籤截斷與 `+N`、窄欄裁切、選取列不位移 |
| graph 開關（P6） | 標題列三種狀態截圖：graph 開啟（範圍按鈕、強調色開關、重新整理）、使用者關閉（開關、重新整理）、fallback（只有重新整理） |
| fallback 模式 | 驗收 agent 逐項比對：graph 不可用時，標題列與清單的結構、class、`●` 與文字都和 `a89b44e` 相同 |

環境備註：主 crate 的 `supported_agents_native_reads_frontmatter` 與 `bat-agent-bridge` 的 `end_to_end_bundled_sdk_loads_through_bundled_node` 會讀使用者家目錄，在 PowerShell 環境可能失敗；這與本功能無關，在 Git Bash 下以同一份程式碼重跑會通過。

### 11.3 未驗證

- 遠端模式端對端（新 client 對新 host、新 client 對舊 host）。
- macOS WKWebView、Linux WebKitGTK 的呈現。
- 在執行中的 app 內實際操作（使用者正在使用 BAT，app 為單一實例）。
- 沒有 commit-graph 檔的超大 repo 的實際耗時。
- 接近 5 MiB 上限的 diff 在 diff 畫面的繪製效能（P5 之後才會顯示出來）。
