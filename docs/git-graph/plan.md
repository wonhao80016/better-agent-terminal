# Git Graph 規劃

Git 頁面的 commit 清單加上分支線圖（git graph），呈現方式參考 VS Code Git Graph：每個 commit 是一個節點，分支、合併以彩色車道線連接，並在訊息前標示 branch 與 tag。

- 狀態：P0–P9 已完成並通過驗證（第 11 節）；第 9 節 D1–D7 已由使用者確認（2026-11-11）。
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
- 逾時：以 `ProcessTree::kill` 結束整個行程樹（P7，D6），然後**立即回傳 `None`，不 join 讀取執行緒**。Job Object 無法建立時會退回只結束直接子行程；這時 wrapper 啟動的真正 git 可能仍存活並持有 pipe，join 會讓呼叫端一直等下去。
- 正常結束：join 讀取執行緒；exit code 非 0、讀取失敗或超過上限都回 `None`。

修正後，超過門檻的 diff 會實際顯示出來；接近 5 MiB 上限的 diff 可能讓 diff 畫面變慢。worktree 功能另有一個私有的同名函式（`src-tauri/crates/bat-git/src/worktree.rs:194`），「等結束才讀」的寫法相同，不在 D2 範圍內，未修改其讀取方式；它的逾時處理已在 P7 改用 `ProcessTree`。

`ProcessTree`（`src-tauri/crates/bat-host-support/src/subprocess.rs`）在 Windows 上先建立「關閉時結束全部」的 Job Object，再以 `CREATE_SUSPENDED` 建立子行程、放進 Job 後才以 ntdll 的 `NtResumeProcess` 恢復執行，所以子行程啟動的任何行程都一定屬於這個 Job。`NtResumeProcess` 沒有正式文件，但從 Windows XP 起就一直穩定存在；改用它之前，以 Toolhelp 掃描全系統執行緒來找出要恢復的那一條，每次啟動多花約 200 ms，改用後只多約 4 ms（30 次中位數 71.7 ms，一般 spawn 67.6 ms）。`kill()` 或物件被丟棄時，Job 內所有行程（含仍處於暫停狀態的）都會結束。子行程無法恢復時會被結束並回報錯誤，絕不留下暫停的行程；Job 無法建立時退回原本的做法。副作用：git 自己啟動、原本會常駐的背景程式（例如啟用 `core.fsmonitor` 時的 fsmonitor 常駐程式）會隨該次呼叫結束，下次再啟動。

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
| 不是 Git repo 時 | 訊息下方加「重新整理」按鈕（沿用 `git.refresh`），與標題列的重新整理相同（P9） |

新增 i18n key（四個語系）：`git.scopeCurrent`、`git.scopeAll`、`git.scopeToggleTitle`、`git.loadMore`、`git.showGraph`、`git.hideGraph`。

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
| P5（D2，`f68418d`） | `run_git` 改為邊讀邊等，`run_git_capture` 併入；大輸出測試改測 `run_git` 並加入大 diff | 修正前後各跑一次該測試；`cargo check`、headless check、workspace 測試 |
| P6（D3，`75fb78c`） | graph 開關、設定值解析與測試、i18n | tsc、compile、`test:git-graph`、標題列截圖 |
| P7（D6，`25f8b31`、`65135e9`） | `bat-host-support` 新增 `ProcessTree`（`process-tree` 功能，預設開啟）；5 個逾時會結束 git 的 runner 改用它 | Windows 行程樹測試（含 git shim）、修正前後的孤兒重現、workspace 測試、獨立驗收 |
| P8（D7，`ede8885`、`6d9f789`） | `bat-git` 新增 `head::lookup_head`，`git_get_branch_native` 先讀 repo 檔案，有任何疑慮才執行 git | 檔案結構單元測試、與 `git rev-parse` 逐狀態比對的實際 repo 測試、本機 4 個工作區比對、獨立驗收（77 種情境） |
| P9（D7，`e3dbe07`） | 「Not a git repository」畫面加重新整理按鈕 | tsc、compile、畫面截圖 |

## 8. 風險

| # | 風險 | 對策 |
|---|---|---|
| R1 | 非常大的 repo 沒有 commit-graph 檔時，`--date-order` 可能要走訪大量歷史而逾時 | 原清單照常先顯示（I7）；同一路徑本工作階段不再重試 |
| R2 | 遠端連到舊版 host | 新 channel 失敗即維持原清單；每個 path 每工作階段最多送一次 |
| R3 | commit 欄很窄時 graph 佔太多空間 | graph 寬度上限為列寬 40%，超出裁切，被裁掉欄位的節點不顯示 |
| R4 | macOS WKWebView、Linux WebKitGTK 的呈現差異 | 只用標準 SVG 屬性與 CSS `min()`；無實機可驗，列為限制 |
| R5 | graph 模式下選取列的左側色條改用 box-shadow | 視覺相同、不位移；fallback 模式不受影響 |
| R6 | 逾時時 Windows 上殘留真正的 git 行程 | 已由 D6 修正（P7）：所有 git runner 改用 `ProcessTree`，逾時時以 Job Object 結束整個行程樹，包括仍處於暫停狀態的子行程。原因與證據見 11.4。修正前，本機 2026-11-11 量到 117 個暫停的孤兒 `git rev-parse --abbrev-ref HEAD`（約 460 MB），清除後一小時內又累積 45 個 |

## 9. 使用者決策

以下項目由使用者於 2026-11-11 決定：

| # | 決策 | 結果 |
|---|---|---|
| D1 | 預設顯示範圍：目前分支，或全部分支（附圖的樣子） | 目前分支（維持現有清單內容）；可切換並記住 |
| D2 | 是否修正既有 `run_git` 大輸出卡死（超過約 64 KB 的 diff 會變成空白） | 修正，在本分支以獨立 commit 進行（5.2、P5） |
| D3 | 是否需要完全關閉 graph 的開關 | 加入標題列開關，關閉時回到原本的清單樣式並記住選擇（5.6、P6） |
| D5 | 「全部分支」是否包含遠端分支與 tag | 包含，與附圖一致 |
| D6 | 逾時只結束 wrapper、卡住的 git 永遠殘留：是否清除現有孤兒、修正、追查原因 | 三項都做：清除本機的孤兒；以 `ProcessTree`（Job Object）修正所有 git runner（P7）；原因見 11.4 |
| D7 | 讀分支造成的行程數量，以及它逾時讓 Git 頁面誤判「Not a git repository」（11.5） | 讀分支改為直接讀 repo 檔案、必要時才退回 git（P8）；「Not a git repository」畫面加重新整理按鈕（P9） |
| D8（2026-11-13） | 歷史 commit 的 File 檢視沒有變更標示、改名檔案（`R<相似度>`）看不到 diff | File 檢視改讀該 commit 的檔案內容（`git cat-file blob <hash>:<path>`，新指令 `git_show_file`／遠端 `git:show-file`），並套用與 Diff 分頁相同的 diff；刪除的檔案顯示刪除前內容、整份標為刪除；改名與複製拆成新舊路徑，以 blob 對 blob 的 `git diff <hash>~1:<舊> <hash>:<新>` 取 diff（不混入舊路徑本身的修改），清單顯示「舊路徑 → 新路徑」；遠端 host 沒有 `git:show-file` 時改讀工作目錄檔案、不加標示。檔案清單與未提交變更改用 `-z` 輸出解析，非 ASCII 與特殊字元路徑不再被 C-quote 破壞，暫存的改名也拆成新舊路徑；diff 的 pathspec 一律用 `:(top,literal)<path>`，workspace 位於 repo 子目錄時仍正確，且 `*`、`?`、`[` 不會被當成萬用字元 |
| D9（2026-11-13） | commit 列改成手風琴展開，並移除中間的檔案欄 | Git 頁面改為兩欄（commit 清單＋Diff/File）；點 commit 列在其下方展開明細，一次只展開一列、再點一次收合；明細依序為完整 hash、parents（可點擊跳到該 commit 並展開，不在目前載入清單內的 parent 顯示為不可點擊）、Author／Date、與 Author 不同時才顯示的 Committer／Date、完整訊息，最後是變更檔案清單；Uncommitted 列只顯示變更檔案清單。明細由新指令 `git_get_commit_details`／遠端 `git:commit-details` 依需求取得（`git log -1 --format` 以 NUL 分隔欄位），每個 hash 在本次面板期間快取；展開區塊中 graph 車道繼續往下延伸 |

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

P8 的獨立驗收（2026-11-11）用真實 git 建立 77 種情境，逐一比對 `lookup_head` 與 `git rev-parse --abbrev-ref HEAD`：55 種相同、10 種交給 git，12 種不同。原則改為「有任何疑慮就交給 git」後修正如下；新增的測試在修正前的實作上有 5 個失敗，修正後全部通過：

| 發現 | 處理 |
|---|---|
| 經由 junction、symlink、`..` 或相對路徑進入時，照字面往上找；git 會先解析成實際路徑（例如 junction 指向另一個 repo 時讀到錯的分支） | 先 `canonicalize` 成實際路徑再往上找；組合路徑時逐段處理，避免 Windows `\\?\` 路徑把 `..` 與 `/` 當成字面 |
| 目前目錄有名為 `HEAD` 的項目時，git 會把 `HEAD` 當成路徑（還沒有 commit 時輸出 `HEAD` 並成功） | 一律交給 git |
| 分支 ref 本身是 symbolic ref、內容為空或損毀、hash 長度與 repo 格式不符 | 驗證 loose ref 與 detached HEAD 的內容是該 repo 長度的 hash（讀 `extensions.objectformat`），否則交給 git |
| `gitdir:` 後沒有空白也被接受，git 會視為格式錯誤 | 和 git 一樣只接受 `gitdir: ` |
| bare repo 受 `safe.bareRepository` 影響；未知的 repo extension 或格式版本會被 git 拒絕 | bare repo、未知 extension、`repositoryformatversion` > 1 都交給 git |
| 背景讀取失敗時直接回傳 `None` | 改為交給 git |
| phase table 沒有 commit hash | 已補上 |

## 11. 實作與驗證結果

### 11.1 實作內容

commit 以 `feat/git-graph` 分支為準，依階段為：`dcfa7be`（P0 規劃）、`9ac1cee`（P1 後端）、`3e83a1d`（P2 資料與演算法）、`2379d3e`（P3 UI）、`5ab31e9`／`1af942d`／`54f9282`（P4 修正與文件）、`f68418d`（P5，D2：`run_git` 大輸出）、`75fb78c`（P6，D3：graph 開關）、`ca2b828`／`04087d7`／`f546808`（決策紀錄與小修）、`25f8b31`（P7，D6：`ProcessTree`）、`65135e9`（P7 效能修正：改用 `NtResumeProcess`）、`775d68f`（文件）、`a11e5a6`（驗收小修）、`ede8885`（P8，D7：直接讀 HEAD）、`e3dbe07`（P9，D7：重新整理按鈕）、`44cde28`（文件）、`6d9f789`（P8 驗收修正：有疑慮就交給 git），之後是本文件的結果更新。

| 層 | 檔案 |
|---|---|
| Rust | `src-tauri/crates/bat-git/src/git.rs`（型別、`run_git`、`build_graph_log_args`、`parse_graph_log` 與測試）、`src-tauri/src/commands/git.rs`、`src-tauri/src/lib.rs`、`src-tauri/src/remote_server.rs`、`src-tauri/crates/bat-remote-protocol/src/remote_core.rs` |
| Rust（P8） | `src-tauri/crates/bat-git/src/head.rs`（`lookup_head` 與測試）、`src-tauri/crates/bat-git/src/lib.rs`、`src-tauri/src/commands/git.rs`（`git_get_branch_native`） |
| Rust（P7） | `src-tauri/crates/bat-host-support/src/subprocess.rs`（`ProcessTree` 與 Windows 測試）、`src-tauri/crates/bat-host-support/Cargo.toml`、`src-tauri/crates/bat-git/src/worktree.rs`、`src-tauri/crates/bat-git/Cargo.toml`、`src-tauri/crates/bat-session-index/src/git.rs`、`src-tauri/crates/bat-session-index/Cargo.toml`、`src-tauri/src/commands/claude.rs` |
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
| `ProcessTree`（P7） | 4 個 Windows 測試：`kill()` 與丟棄物件都會結束孫行程、git shim 背後的真正 git 會一起結束（在系統 PATH 下執行，確認未略過）、管線輸出可正常讀完。對照組：只結束 shim 時，其子行程仍存活 |
| 孤兒重現（P7） | 透過 shim、逾時 1–400 ms 各呼叫 56 次（全部逾時）：舊寫法留下 4 個暫停的孤兒，`run_git` 留下 0 個。獨立驗收另做三組（逾時 1–400 ms 各 192 次、150–400 ms 各 104 次、0–400 ms 結束各 82 次）：舊寫法分別留下 5、22、9 個，`run_git` 都是 0 個。改用 `NtResumeProcess` 後再跑兩輪共 112 次，仍是 0 個 |
| 啟動成本（P7） | 以 Toolhelp 恢復執行緒時，`ProcessTree` 啟動中位數 266 ms、一般 spawn 70 ms；改用 `NtResumeProcess` 後為 71.7 ms 對 67.6 ms |
| `cargo test --workspace --lib`（P7 後） | 747 個全部通過（多出的 4 個是 `ProcessTree` 測試）；`Cargo.lock` 沒有變動 |
| 直接讀 HEAD（P8） | 9 個 `head` 測試通過，其中實際 repo 測試在每個狀態（還沒有 commit、一般分支、子目錄、名稱含 `/`、packed-refs、linked worktree、detached HEAD）都與 `git rev-parse --abbrev-ref HEAD` 相同；分支與 tag 同名時退回 git，git 輸出 `heads/feat/x` |
| 本機工作區比對（P8） | BAT 設定的 4 個工作區結果都與 git 相同：`moc_refactor` `main`、`driver` `fix/s4-resume-pending-capture`、`better-agent-terminal` `feat/git-graph`、`d:\claude` 不是 repo；每次 0.4–10 ms，git 為 0.9–1.1 秒 |
| 重新整理按鈕（P9） | tsc、compile 與 renderer 測試通過；截圖確認訊息下方置中顯示按鈕 |
| `cargo test --workspace --lib`（P8 後） | 756 個全部通過（多出的 9 個是 `head` 測試）；P8 驗收修正後為 758 個（`head` 測試增為 11 個，含 junction 與真實 repo 的 HEAD 檔案情境；修正前的實作有其中 5 個失敗）；`Cargo.lock` 沒有變動 |

環境備註：主 crate 的 `supported_agents_native_reads_frontmatter` 與 `bat-agent-bridge` 的 `end_to_end_bundled_sdk_loads_through_bundled_node` 會讀使用者家目錄，在 PowerShell 環境可能失敗；`bat-session-index` 有 3 個測試在 TEMP 為 8.3 短路徑時失敗（`a89b44e` 之前就如此）。這些都與本功能無關，在 Git Bash、長路徑 TEMP 下以同一份程式碼重跑會通過。測試 `ProcessTree` 的 git shim 情境要在系統 PATH（PowerShell）下執行，Git Bash 的 PATH 會直接找到真正的 git 而略過該測試。

### 11.3 未驗證

- 遠端模式端對端（新 client 對新 host、新 client 對舊 host）。
- macOS WKWebView、Linux WebKitGTK 的呈現。
- 在執行中的 app 內實際操作（使用者正在使用 BAT，app 為單一實例）。
- 沒有 commit-graph 檔的超大 repo 的實際耗時。
- 接近 5 MiB 上限的 diff 在 diff 畫面的繪製效能（P5 之後才會顯示出來）。
- P7 要重新建置並重啟 BAT 才會生效；執行中的舊版仍會繼續產生孤兒。其他工具（例如 VS Code）留下的孤兒不受影響。

### 11.4 D6 根本原因調查摘要（2026-11-11）

- 孤兒的特徵：映像為 `ucrt64\bin\git.exe`，只有 1 條執行緒，處於 Wait/Suspended（suspend count 1），CPU 0，從未恢復執行；父行程（`cmd\git.exe` wrapper）已結束，並各留下一個孤兒 `conhost.exe`。
- 機制：Windows 的 CreateProcess 會先以暫停狀態建立子行程的初始執行緒，建立完成時才由建立者恢復。wrapper 的 `CreateProcessW` 尚未完成時就被 3 秒逾時結束，子行程便停在暫停狀態。
- 在本機，這段建立時間被安全軟體（Trend Micro Apex One、Trellix DLP）的行程建立攔截拉長。高負載時 wrapper 建立子行程要 2–3 秒，正好落在 3 秒逾時上；在一般負載下這段時間約 60–175 ms。
- 實驗室負載下，建立者在 `CreateProcessW` 返回前被結束時，子行程通常會由系統清掉（`0xC000010A`）；現場為何沒有被清掉，未查明。但 Job Object 的做法不依賴這個清理：模擬實驗 15/15 次都結束了子行程，不用 Job 的對照組 3/3 留下孤兒。
- 家目錄位於網路磁碟（`Z:`）：global config 確實讀 `Z:` 上的檔案，但對啟動時間的影響在誤差範圍內（中位數差 15–60 ms），不是主因。
- 呼叫頻率：Claude 與 Codex agent 面板每 5 秒讀一次分支（`renderer/src/components/ClaudeAgentPanel.tsx`、`CodexAgentPanel.tsx`），沒有防重入；實測每分鐘 12 次，每次啟動 wrapper、conhost、git 三個行程。這是 D7 的背景。

### 11.5 D7：Git 頁面誤判「Not a git repository」（2026-11-11）

- 現象：`moc_refactor` 工作區的 Git 頁面顯示「Not a git repository」，但該目錄是正常的 git repo（`.git` 存在，`git rev-parse --abbrev-ref HEAD` 成功回傳 `main`，沒有權限警告）。
- 原因：Git 頁面以讀分支（`git_get_branch`，逾時 3 秒）是否成功判斷是不是 repo，而且開啟時同時啟動 log、status、讀分支、root 四個 git。在本機，即使 CPU 只有 11%，讀分支也要 1.8–2.0 秒；系統較忙時就會超過 3 秒而被誤判。該畫面原本沒有重新整理按鈕，只能切換分頁才會重新判斷。截圖當下的那次呼叫沒有紀錄，所以「是逾時」是排除其他原因後的推論。
- 處理：讀分支改為直接讀 repo 檔案（P8），不再受行程啟動時間影響，agent 面板的輪詢也不再啟動行程；「Not a git repository」畫面加重新整理按鈕（P9）。
- 行為差異：判斷分支不再需要 git，所以在 git 會拒絕的 repo（例如 `safe.directory` 的 dubious ownership）或沒有安裝 git 的電腦上，Git 頁面會顯示空的清單，而不是「Not a git repository」。
