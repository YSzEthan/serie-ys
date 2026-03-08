---
name: graph-test
description: "執行圖形渲染整合測試並檢查輸出快照。當使用者想要測試圖形渲染、驗證圖形變更、或執行圖形相關測試時使用（例如：'測試圖形'、'執行 graph 測試'、'檢查圖形渲染'）。"
---

# 圖形測試

執行 `tests/graph.rs` 中的圖形渲染整合測試。

## 步驟

1. 執行所有圖形測試：
   ```bash
   cargo test --test graph --verbose
   ```

2. 執行特定圖形測試：
   ```bash
   cargo test --test graph <test_name> --verbose
   ```

3. 測試完成後，告知使用者視覺快照已儲存至 `./out/graph/`，可供手動檢查。

4. 若使用者想檢視特定快照，在 macOS 上使用 `open ./out/graph/<名稱>.png`。
