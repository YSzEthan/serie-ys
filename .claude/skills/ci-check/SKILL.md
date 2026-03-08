---
name: ci-check
description: "在本地執行完整 CI 流程：格式檢查、建置、clippy lint 和測試。當使用者想要在提交前驗證程式碼、在本地執行 CI、或檢查所有項目是否通過時使用（例如：'執行 CI'、'檢查全部'、'提交前檢查'）。"
---

# CI 檢查

執行與 GitHub Actions `build.yml` 工作流程一致的完整 CI 流程。

## 步驟

依序執行，遇到失敗即停止：

```bash
cargo fmt --all -- --check
cargo build --verbose
cargo clippy --all-targets --all-features -- -D warnings
cargo test --verbose
```

回報每個步驟的通過/失敗狀態。失敗時顯示錯誤輸出並建議修正方式。
