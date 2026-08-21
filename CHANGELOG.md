# Changelog

## [3.4.0](https://github.com/YSzEthan/serie-ys/compare/v3.3.1...v3.4.0) (2026-08-21)


### Features

* 新增內嵌命令列，支援 Working changes 列與使用者 shell alias (#79) (#80) ([047dd7c](https://github.com/YSzEthan/serie-ys/commit/047dd7cc1a7c6a47c2c41eee22328bc817f0642c))

## [3.3.1](https://github.com/YSzEthan/serie-ys/compare/v3.3.0...v3.3.1) (2026-08-16)


### Bug Fixes

* 自動更新加上下載中提示，並避免凍結使用者輸入 (#77) (#78) ([c2c42c5](https://github.com/YSzEthan/serie-ys/commit/c2c42c5385f985deeae0289ce96c9e98ae9bf885))

## [3.3.0](https://github.com/YSzEthan/serie-ys/compare/v3.2.0...v3.3.0) (2026-08-16)


### Features

* Release Notes 內文限制閱讀寬度並水平置中 (#75) (#76) ([f82ab98](https://github.com/YSzEthan/serie-ys/commit/f82ab98dc0d692a7ce7667e854490bab765126e3))

## [3.2.0](https://github.com/YSzEthan/serie-ys/compare/v3.1.0...v3.2.0) (2026-08-16)


### Features

* 更新後首次啟動自動顯示 Release Notes (#73) (#74) ([fa3e8f0](https://github.com/YSzEthan/serie-ys/commit/fa3e8f0375a7eacd63ed299ce7876321b5a9327d))

## [3.1.0](https://github.com/YSzEthan/serie-ys/compare/v3.0.0...v3.1.0) (2026-08-15)


### Features

* gh detail commit 集中成一區，簡化分隔線邏輯 (#71) (#72) ([c1db811](https://github.com/YSzEthan/serie-ys/commit/c1db811717be6bcbe35d7a80a94dd6cebb32d43a))

## [3.0.0](https://github.com/YSzEthan/serie-ys/compare/v2.7.3...v3.0.0) (2026-08-13)


### ⚠ BREAKING CHANGES

* [keybind] 底下的 action 一旦被使用者設定快捷鍵，會完全 取代它的預設鍵位，不再是疊加。例如 navigate_down = ["ctrl-n"] 之後，預設 的 j/down 不再對 navigate_down 生效；把快捷鍵設成 [] 則是這個規則的特例， 用來明確停用一個 action（過去文件宣稱但沒有實作）。

### Features

* wizard 架構重構 + 顏色／keybind 編輯器 (#61)(#66)(#69)(#70) (#68) ([badd122](https://github.com/YSzEthan/serie-ys/commit/badd12286c2fa4cba4bb6568775554a5885d6980))

## [2.7.3](https://github.com/YSzEthan/serie-ys/compare/v2.7.2...v2.7.3) (2026-08-12)


### Refactors

* TOML 架構重整，補漏鍵、合併 [ui.*] 單鍵區塊、[graph.color] 歸位到 [color.graph] (#60) (#64) ([6383bad](https://github.com/YSzEthan/serie-ys/commit/6383badf51deb73f2d2c8d0adb1890b3e8426ec1))


### CI

* 版號規範補齊 11 種 type，格式檢查前移到 lefthook 與 PR 標題 (#65) ([7995aa9](https://github.com/YSzEthan/serie-ys/commit/7995aa9941ba2af82ada696441cfec5d8363fd5a))

## [2.7.2](https://github.com/YSzEthan/serie-ys/compare/v2.7.1...v2.7.2) (2026-08-11)


### Bug Fixes

* 以 exe_is_stale() 收斂更新狀態判斷，解決多實例重複下載 (#59) (#63) ([cc2c43e](https://github.com/YSzEthan/serie-ys/commit/cc2c43e84a528b1d94212e934b9473b14ce0fcc9))

## [2.7.1](https://github.com/YSzEthan/serie-ys/compare/v2.7.0...v2.7.1) (2026-08-11)


### Bug Fixes

* gh CLI 呼叫加 timeout，並優化 GitHub 資料載入效率 (#57) (#58) ([ec7d8d7](https://github.com/YSzEthan/serie-ys/commit/ec7d8d7e4ecfbd9ba9fca9715fa68f38e9cd1254))

## [2.7.0](https://github.com/YSzEthan/serie-ys/compare/v2.6.0...v2.7.0) (2026-08-11)


### Features

* 自動更新新增三態模式、可調間隔、自動重啟，設定檔跟著執行檔走 (#55) (#56) ([68da156](https://github.com/YSzEthan/serie-ys/commit/68da15602e40ad314b237c7657c7cbea874ced5c))

## [2.6.0](https://github.com/YSzEthan/serie-ys/compare/v2.5.1...v2.6.0) (2026-08-10)


### Features

* -V 列出所有選項、自我更新加入 y/n 確認 (#53) (#54) ([169909b](https://github.com/YSzEthan/serie-ys/commit/169909b067bd4bfccac8d928de7e1cc7201191b9))


### Refactors

* truncate_line 兩條路徑併回一條，刪掉 str_width (#52) ([0b847d4](https://github.com/YSzEthan/serie-ys/commit/0b847d4a1cb226c465e76768c0e45f2b33ca7397))

## [2.5.1](https://github.com/YSzEthan/serie-ys/compare/v2.5.0...v2.5.1) (2026-08-10)


### Bug Fixes

* GitHub view 提示列格式、Vercel HTML 留言、SSH/mosh 開連結三處修正 (#51) ([7f2433b](https://github.com/YSzEthan/serie-ys/commit/7f2433be5b02360eba99c778ee6bebd200eae2f6))

## [2.5.0](https://github.com/YSzEthan/serie-ys/compare/v2.4.1...v2.5.0) (2026-08-10)


### Features

* 新增自我更新功能，偵測 GitHub Release 新版並就地替換執行檔 (#50) ([fd4ad7a](https://github.com/YSzEthan/serie-ys/commit/fd4ad7ad1f4554b6cd42403648c37a5206622da5))

## [2.4.1](https://github.com/YSzEthan/serie-ys/compare/v2.4.0...v2.4.1) (2026-08-09)


### Bug Fixes

* diff pane 在每個 hunk 之間加分隔線 (#49) ([d6bae93](https://github.com/YSzEthan/serie-ys/commit/d6bae93790c6769dc4e67dc304f9c6f623e33947))

## [2.4.0](https://github.com/YSzEthan/serie-ys/compare/v2.3.1...v2.4.0) (2026-08-09)


### Features

* detail view diff pane 重做，自訂行號／header／行內差異／hunk 導航 (#47) (#48) ([4143143](https://github.com/YSzEthan/serie-ys/commit/41431432074a8de88af7835aaa47a826f9f2b41b))

## [2.3.1](https://github.com/YSzEthan/serie-ys/compare/v2.3.0...v2.3.1) (2026-08-08)


### Bug Fixes

* issue detail 的 timeline 查詢不再帶 PR 專屬片段 (#46) ([92043f2](https://github.com/YSzEthan/serie-ys/commit/92043f2715660ca7795ef1cefd64403b838512a0))

## [2.3.0](https://github.com/YSzEthan/serie-ys/compare/v2.2.1...v2.3.0) (2026-08-08)


### Features

* detail view 單檔 diff 預覽，狀態列提示改由 keybind 動態產生 (#43, #44) (#45) ([6222485](https://github.com/YSzEthan/serie-ys/commit/6222485472cce3f63f8b6e492590afe143b75c2b))

## [2.2.1](https://github.com/YSzEthan/serie-ys/compare/v2.2.0...v2.2.1) (2026-08-07)


### Bug Fixes

* GitHub Release 補上 CHANGELOG 區塊當說明文字 ([#42](https://github.com/YSzEthan/serie-ys/pull/42))

## [2.2.0](https://github.com/YSzEthan/serie-ys/compare/v2.1.0...v2.2.0) (2026-08-07)


### Features

* GitHub 視圖按 r 一併刷新 commit CI 狀態 (#39) ([#40](https://github.com/YSzEthan/serie-ys/pull/40))

## [2.1.0](https://github.com/YSzEthan/serie-ys/compare/v2.0.0...v2.1.0) (2026-08-07)


### Features

* merge 進 main 後自動算版號、寫 CHANGELOG、掛 tag 並發版 ([#37](https://github.com/YSzEthan/serie-ys/pull/37))


### Bug Fixes

* release CI 的 Cargo.lock 同步改直接改欄位，不呼叫 cargo metadata ([#38](https://github.com/YSzEthan/serie-ys/pull/38))

## [2.0.0](https://github.com/YSzEthan/serie-ys/compare/v1.9.0...v2.0.0) (2026-08-07)


### ⚠ BREAKING CHANGES

* 無對外相容性影響（CLI flag／設定檔 schema 都沒變）， 標記為 major 純粹是這次 GitHub 狀態管理架構重寫的版本里程碑判斷。

### Features

* --help/-V 說明文字改正體中文 ([b756d3c](https://github.com/YSzEthan/serie-ys/commit/b756d3c789ab5fbb6d0e08be17e569b0ab116dd5))
* -h 在 TTY 下改成互動選單，新增 -p 目錄瀏覽器 ([09351ef](https://github.com/YSzEthan/serie-ys/commit/09351efd839abe2536dac816531d4151180e48c9))
* double 改成只在不損失資訊時才合併同一欄的線 ([81d3590](https://github.com/YSzEthan/serie-ys/commit/81d359099387c4acf8a56a6eba9e4330089e4c5d))
* graph 寬度拆成 double-l／double-f，double-f 不再吃掉連線 ([3fb92e8](https://github.com/YSzEthan/serie-ys/commit/3fb92e89d78dcc781b5f46f5eedce4b732416225))
* single 寬度改用 box-drawing 接點字元，線不再互相吃掉 ([b8ea82b](https://github.com/YSzEthan/serie-ys/commit/b8ea82b27ed9834ee600034381421ac79eccbd6e)), closes [#29](https://github.com/YSzEthan/serie-ys/issues/29)
* 修掉 GitHub filter 切換的兩份 bug，順手解決 [#27](https://github.com/YSzEthan/serie-ys/issues/27) ([39aaca9](https://github.com/YSzEthan/serie-ys/commit/39aaca97f3cc2a38eb361eb098035206a6f0c292))
* 接上 GraphStyle，-s 旗標真正生效（graph 欄／marker 欄／inline detail 邊框） ([8b0f669](https://github.com/YSzEthan/serie-ys/commit/8b0f6691c84c261cd15d0231930e9ed4a680248e))
* 新增緊湊模式（-c/--compact），commit 文字貼齊該列 graph 實際延伸的位置 ([7f8bf59](https://github.com/YSzEthan/serie-ys/commit/7f8bf5988843f032867173ac52d0a9e7e7a56dcc))


### Bug Fixes

* --help 的 graph_width 敘述停在圖片時代，[PATH] 預設值印兩次 ([ffd341a](https://github.com/YSzEthan/serie-ys/commit/ffd341ad887e85f07b4f2261f138ebfcd7636735))
* exclude 排掉 /docs 讓打包出來的 crate 測試編不過 ([0afb292](https://github.com/YSzEthan/serie-ys/commit/0afb292cea491421c24e7906d0f2dfd8a773db5c))
* filter/search 輸入框打不出 g 與 ?，被 app 層攔去開 GitHub 與說明頁 ([a0aa105](https://github.com/YSzEthan/serie-ys/commit/a0aa105ac2a25e9d1b4df602231eda2f66703e53))
* keybind 解析用 byte 長度判斷單一字元，非 ASCII 鍵全被拒絕 ([1ed251e](https://github.com/YSzEthan/serie-ys/commit/1ed251e9810b6d8d384110b5b6d74434a70cc2bd))
* MSRV clippy 卡 uninlined_format_args，CI 重複跑兩次 ([b5124d1](https://github.com/YSzEthan/serie-ys/commit/b5124d1a06c7059d12b2e8cdd5afd14def3c9a04))
* spacer row 不再幫 ╰／╯ 多畫一條線，全 repo 註解改正體中文 ([60bc38b](https://github.com/YSzEthan/serie-ys/commit/60bc38b8e76263031df96cf2fa10736d7ed3bb39))
* ui.list.columns 預設順序在文件與 schema 都寫錯，補上釘住文件範例的測試 ([9eba061](https://github.com/YSzEthan/serie-ys/commit/9eba06190a27de3c4cec687da7c0f1845bdb1b2b))
* 窄終端不再拒絕啟動，改為降級或截斷（[#21](https://github.com/YSzEthan/serie-ys/issues/21)） ([daa49a8](https://github.com/YSzEthan/serie-ys/commit/daa49a8279f51e1edc82692f562bb02a396b560f))


### Refactors

* /simplify 收尾 —— 修正折疊不變式錯誤註解，補長度斷言，消掉一次 tuple round-trip ([9957957](https://github.com/YSzEthan/serie-ys/commit/99579572ed4f506711308b3444394a03a03da0b3))
* /simplify 收尾 —— 清掉 [#19](https://github.com/YSzEthan/serie-ys/issues/19) 三個 commit 留下的死欄位與重複結構 ([a7c2717](https://github.com/YSzEthan/serie-ys/commit/a7c2717447f02415de48bccab77fbbf61277a4fb))
* GlyphSet 欄位型別從 char 改成 &'static str ([8c9facc](https://github.com/YSzEthan/serie-ys/commit/8c9facce80122cdb4d17882cbb3833702869d2dc))
* PreviewCache 收成自帶失效判斷的獨立型別 ([0ceb888](https://github.com/YSzEthan/serie-ys/commit/0ceb88880b9a773250105f8b838d2f858e24faec)), closes [#24](https://github.com/YSzEthan/serie-ys/issues/24)
* TextCell 改存語義 Glyph，引入 GlyphSet ([c8179f0](https://github.com/YSzEthan/serie-ys/commit/c8179f0ddb458d3c25eecce50f2c4685bb3165f3)), closes [#19](https://github.com/YSzEthan/serie-ys/issues/19)
* 刪除 GraphColorSet 的死顏色欄位與 alpha 支援 ([95bbcf3](https://github.com/YSzEthan/serie-ys/commit/95bbcf37e4a626de8e89a889eb3ff5fd2e705108))
* 刪除 PNG 圖片渲染實作，只保留文字繪圖路徑 ([c8abdf3](https://github.com/YSzEthan/serie-ys/commit/c8abdf3145abd1653903ae307d41835e604b9468)), closes [#18](https://github.com/YSzEthan/serie-ys/issues/18)
* 合併 GraphStyle 兩份定義，加 Ascii 與 ANGULAR/ASCII 兩張 GlyphSet ([62d7018](https://github.com/YSzEthan/serie-ys/commit/62d701804ff25f30fce29ddfbd5a9c8a4a456d21))
* 從 App 抽出 StatusLineState，狀態機獨立成 src/app/status_line.rs ([f340a9d](https://github.com/YSzEthan/serie-ys/commit/f340a9d22ea17fab31cbc8441aee80f8cb0edb06)), closes [#26](https://github.com/YSzEthan/serie-ys/issues/26)
* 把「一個 graph 欄佔幾格」收斂成單一真相 ([dcd9413](https://github.com/YSzEthan/serie-ys/commit/dcd94133f79187b4c24591b36a7ec3106f2722a7))
* 拆分 view/github.rs（純搬家） ([2542694](https://github.com/YSzEthan/serie-ys/commit/25426947e644f86db59f60db18737090db4f81a3)), closes [#23](https://github.com/YSzEthan/serie-ys/issues/23)
* 拆分 widget/commit_list.rs（純搬家） ([3e5ea1f](https://github.com/YSzEthan/serie-ys/commit/3e5ea1f97075b4289efa2a0cd2d505d1b1ec5832)), closes [#25](https://github.com/YSzEthan/serie-ys/issues/25)
* 拆掉 GraphImageManager，欄位收進 CommitListState ([d44b86d](https://github.com/YSzEthan/serie-ys/commit/d44b86d2f5a438e69ae55fc406c8827b24de81c0))
* 移除圖片協議選擇入口，固定為文字模式 ([acaede1](https://github.com/YSzEthan/serie-ys/commit/acaede1f2dd77a6a9059b4af5588421034612dca)), closes [#17](https://github.com/YSzEthan/serie-ys/issues/17)

## [1.9.0](https://github.com/YSzEthan/serie-ys/compare/v1.8.0...v1.9.0) (2026-07-31)


### Features

* gh 預覽區 commit 記錄可用 z 鍵整體摺疊 ([ab0c37d](https://github.com/YSzEthan/serie-ys/commit/ab0c37de8e09ba99933363e2c6e25933f32a39d7))
* gh 預覽區分隔線依區段上色 ([74d3181](https://github.com/YSzEthan/serie-ys/commit/74d3181af5fde9f1fc660f7671bf79f280db8949))
* gh 預覽區改用 timelineItems，留言與 commit 依時間交錯顯示 ([6928e02](https://github.com/YSzEthan/serie-ys/commit/6928e02fa1f7add483cbb8647a7802a6d97cd53f))
* gh 預覽區顯示 PR 是否可以 merge ([f932c4a](https://github.com/YSzEthan/serie-ys/commit/f932c4a9bc8d08f3423af7cf92d99dfbce5b5919))
* 全畫面展開 GitHub emoji shortcode ([b2ae26d](https://github.com/YSzEthan/serie-ys/commit/b2ae26de105ca7c17a8ccc4d40964cee9dd8bd90))
* 分隔線改用粉彩色調（藍灰 146、綠灰 151） ([d57e539](https://github.com/YSzEthan/serie-ys/commit/d57e5392b156aadccad195155cc2bb4f3f73ff31))


### Bug Fixes

* fuzzy 搜尋 highlight 的索引錯位 ([026006a](https://github.com/YSzEthan/serie-ys/commit/026006a84635ed0ba04bc37111ac6227b6bc7606))
* gh 預覽區捲不到底，改由 Paragraph::scroll 處理捲動 ([dc38baa](https://github.com/YSzEthan/serie-ys/commit/dc38baad869d127836af4840d3ddf7e3a6bfe8a3))
* markdown renderer 濾除 bot 留言雜訊，表格改按實際寬度排版 ([3898be4](https://github.com/YSzEthan/serie-ys/commit/3898be4f803dccf50ab1f10d0e206c1bac568c75))
* suspend 期間收到 SIGTERM 不再拆掉外部程式的終端 ([b852ef3](https://github.com/YSzEthan/serie-ys/commit/b852ef3810a3347a11d870006694d7e17967309f))
* terminal 關閉後 event thread 空轉吃滿 CPU ([35a67af](https://github.com/YSzEthan/serie-ys/commit/35a67afa6c06442c284f5ba88a4630531ab42931))
* 大小寫不敏感搜尋的 highlight 標到隔壁的字 ([16e0ec4](https://github.com/YSzEthan/serie-ys/commit/16e0ec4da16e0cab8441245174c1ee4341b1ac6b))
* 帶 variation selector 的 emoji 讓 marquee 卡死並持續重繪 ([ce0a1b0](https://github.com/YSzEthan/serie-ys/commit/ce0a1b03a5f97167275acb63702c53f652875acd))


### Performance

* 搜尋時不再為每個 commit 重建 SearchMatcher ([47aeb9b](https://github.com/YSzEthan/serie-ys/commit/47aeb9b0b8d48d25ac7e1b34cfc786896ee8b5a9))


### Refactors

* gh 預覽內容改用 PreviewInput 統一輸入來源，comments 全面改名為 timeline ([c86087d](https://github.com/YSzEthan/serie-ys/commit/c86087d6d952bbb333c68b5adc5c86ad043eaa70))
* 搜尋路徑不再走 commit_quick_matches，欄位清單收成一份 ([59e7133](https://github.com/YSzEthan/serie-ys/commit/59e7133bd57f4abe75aab08927aefdceba7234d1))

## [1.8.0](https://github.com/YSzEthan/serie-ys/compare/v1.7.0...v1.8.0) (2026-07-29)


### Features

* commit list 新增一鍵回到 HEAD（.） ([cb8fadf](https://github.com/YSzEthan/serie-ys/commit/cb8fadf2eaa9e720777573bcd86a8ea39821a029))
* commit list 的 shift-j/shift-k 改為捲動圖表，並綁 e 開啟 git diff ([2976a9b](https://github.com/YSzEthan/serie-ys/commit/2976a9b8cea8a74f442897486db6c2fd21ed4cf9))


### Bug Fixes

* commit list「回到 HEAD」改為比照捲動 margin 移動，不再把 HEAD 硬拉到頂端 ([4d07837](https://github.com/YSzEthan/serie-ys/commit/4d0783765d1e44a906c0074261b3d971e7d0243b))

## [1.7.0](https://github.com/YSzEthan/serie-ys/compare/v1.6.1...v1.7.0) (2026-07-21)


### Features

* GitHub view 新增 draft PR 定案／打回草稿（shift-p） ([febebee](https://github.com/YSzEthan/serie-ys/commit/febebee25f2375ed2ac4b5509df5288686c66e33))
* GitHub view 的 PR 分頁支援 close/reopen（shift-x） ([47226b4](https://github.com/YSzEthan/serie-ys/commit/47226b40b4b0edd4f2db3ce5bad71ee41947fa38))


### Bug Fixes

* merged PR 按狀態切換鍵時補上提示 ([d0c724d](https://github.com/YSzEthan/serie-ys/commit/d0c724d3cc7a2c6e27e4cdcf856d4375d26bdd39))
* 鍵位說明與實作對齊，官網文件改由 in-app help 產生 ([362220c](https://github.com/YSzEthan/serie-ys/commit/362220c1a91fb5f6543cf5e63ae9374c69fd1634))

## [1.6.1](https://github.com/YSzEthan/serie-ys/compare/v1.6.0...v1.6.1) (2026-07-14)


### Bug Fixes

* GitHub view 重新整理時顯示 loading 指示器並修正卡死 ([7d85b01](https://github.com/YSzEthan/serie-ys/commit/7d85b01f6266c32f3f034b9e83219f1c4400133f))

## [1.6.0](https://github.com/YSzEthan/serie-ys/compare/v1.5.1...v1.6.0) (2026-07-11)


### Features

* Shift-C 改為複製 commit subject ([4d632a3](https://github.com/YSzEthan/serie-ys/commit/4d632a3b9500c6c8e39bc7252734e4d90aa11736))

## [1.5.1](https://github.com/YSzEthan/serie-ys/compare/v1.5.0...v1.5.1) (2026-07-06)


### Bug Fixes

* gh pr merge 選不刪除分支不再取消整個 merge ([29b2b3f](https://github.com/YSzEthan/serie-ys/commit/29b2b3f1428c7cfbe69ec9afbffbaeeb48ed93f9))

## [1.5.0](https://github.com/YSzEthan/serie-ys/compare/v1.4.0...v1.5.0) (2026-06-30)


### Features

* 新增 {{stash}} user command 變數 ([5f67b37](https://github.com/YSzEthan/serie-ys/commit/5f67b377195a0881faa7984df439077e9b1080e6))
* 新增 graph.row_image_width 設定（port 上游 [#156](https://github.com/YSzEthan/serie-ys/issues/156)） ([332c469](https://github.com/YSzEthan/serie-ys/commit/332c469815fa6377de7ec494088533834bf80850))


### Bug Fixes

* Detail view commit body 長文字 wrap 顯示 ([9528bbb](https://github.com/YSzEthan/serie-ys/commit/9528bbb383aed5c4d18fdb4912fc327bf7e424d4))
* Detail view commit subject 過長時跑馬燈不動 ([4564807](https://github.com/YSzEthan/serie-ys/commit/456480707e7cafd83e25b7487e85955fce69b62b))
* Detail view header / file tree 的 Span-aware wrap ([1b9ce94](https://github.com/YSzEthan/serie-ys/commit/1b9ce94554450db3b4f715971cd14d47c0733e9d))
* GitHub view text-mode graph 殘留與 tmux 內剪貼簿失效 ([5ec101e](https://github.com/YSzEthan/serie-ys/commit/5ec101e8ebd0259c965bb01053f28a16104fdd9c))
* GitHub view 在 tmux 內 #N 連結錯位至右側 ([705de05](https://github.com/YSzEthan/serie-ys/commit/705de05bde62921559a60eb1debbd947c7c6ae3d))
* GitHub view 重開後 state_filter 未還原為 closed/all ([cb4f72d](https://github.com/YSzEthan/serie-ys/commit/cb4f72d688b2bfe2ac25a4785bf637435f21ea9c))
* tmux nested popup OSC52 改為廣播到所有 client tty ([c37e0da](https://github.com/YSzEthan/serie-ys/commit/c37e0da3b760a6912e549a3874ad4f7c97d93b6f))
* 選取列背景色延伸至 graph 與 marker 欄 ([a5e2b97](https://github.com/YSzEthan/serie-ys/commit/a5e2b97cc691ffb4a5dd9dd6c5c126b17f0fb0ab))

## [1.4.0](https://github.com/YSzEthan/serie-ys/compare/v1.3.0...v1.4.0) (2026-05-12)


### Features

* GitHub view preview 顯示 issue/PR 留言 ([eb1e272](https://github.com/YSzEthan/serie-ys/commit/eb1e27211e6647091739876b954bb1383d856576))
* GitHub view 支援 `:` + 數字跳號到對應 issue/PR ([da68dd4](https://github.com/YSzEthan/serie-ys/commit/da68dd426d198a66d6e41f775288451614d3f1ec))

## [1.3.0](https://github.com/YSzEthan/serie-ys/compare/v1.2.1...v1.3.0) (2026-05-11)


### Features

* GitHub view 支援關閉與重新開啟 issue ([5641a05](https://github.com/YSzEthan/serie-ys/commit/5641a05c947366d6984e7c0a12f3d72e2eb18e8b))


### Bug Fixes

* Detail 高度上限與 scroll clamp 邊界修正 ([9f4fecc](https://github.com/YSzEthan/serie-ys/commit/9f4fecc5270dfea3a6e7f321216c91de6ae4865e))
* PR merge 選擇不刪 branch 時整個流程被取消，並新增衝突提醒 ([ea68f10](https://github.com/YSzEthan/serie-ys/commit/ea68f109cc3ce2e513548f0fb391889d801b185c))
* toggle_issue_state 鍵位由 x 改為 shift-x 避免與 fuzzy_toggle 衝突 ([1cd58cf](https://github.com/YSzEthan/serie-ys/commit/1cd58cf3b2a2f099a7fdc64857f9d60f4865af04))

## [1.2.1](https://github.com/YSzEthan/serie-ys/compare/v1.2.0...v1.2.1) (2026-05-03)


### Bug Fixes

* 互動狀態提示顏色由 DarkGray 改為 Yellow 增加辨識度 ([e24b2d9](https://github.com/YSzEthan/serie-ys/commit/e24b2d99d891654c778ca43b11eda3fd552fde34))

## [1.2.0](https://github.com/YSzEthan/serie-ys/compare/v1.1.0...v1.2.0) (2026-05-03)


### Features

* PR detail 顯示 merge target（base ← head ref） ([549e2d4](https://github.com/YSzEthan/serie-ys/commit/549e2d430832b22ff3ca3526c7a1263921abfffc))


### Bug Fixes

* 互動狀態提示顏色由 DarkGray 改為 Yellow 增加辨識度 ([8ec56fd](https://github.com/YSzEthan/serie-ys/commit/8ec56fdedd81e42c34b24d0c363c7390ca09b580))

## [1.1.0](https://github.com/YSzEthan/serie-ys/compare/v1.0.0...v1.1.0) (2026-05-03)


### Features

* detail 面板 commit subject 加入 marquee 跑馬燈 ([6761d9d](https://github.com/YSzEthan/serie-ys/commit/6761d9d51b32ce0b226bc2da3013ab86b91785b5))
* gh PR view 按 M 三階段 merge PR（merge/squash/rebase） ([6c678e9](https://github.com/YSzEthan/serie-ys/commit/6c678e9c0dcc94303e27fe9aace60dd15cc0a722))
* gh view Preview focus 支援 related-issues 狀態列快速選取 ([d1f3939](https://github.com/YSzEthan/serie-ys/commit/d1f3939c556d0953c0ad5b1e8eb4b5f0fa9f9483))
* GitHub issue/PR 清單 cursor 分頁載入（infinite scroll） ([aa61e61](https://github.com/YSzEthan/serie-ys/commit/aa61e61dbd203bc9aadbf4cbfa5e8cb878adcf6c))
* GitHub related-issues picker、-t 文字模式旗標、hint 提示色統一 ([0619336](https://github.com/YSzEthan/serie-ys/commit/0619336c89f0d35c44dbc3311bcecd7ff850a440))
* list view 按 d 刪除 local branch ([2541099](https://github.com/YSzEthan/serie-ys/commit/2541099dd771f0b0997ef377293cff207138e88b))
* 文字模式 uncommitted ◯ 對齊 HEAD column 並畫灰色連線 ([420b1e8](https://github.com/YSzEthan/serie-ys/commit/420b1e8e95cca5f7ba6aa1a874b061f436f5ec01))


### Bug Fixes

* git status 加 --untracked-files=all，避免新資料夾折疊成空 leaf ([1d1b1b1](https://github.com/YSzEthan/serie-ys/commit/1d1b1b17119a17ba29aeba1e35dd897e4f67c9ff))
* merge_pr keybind 改為 p，去除 shift 需求 ([a239b4e](https://github.com/YSzEthan/serie-ys/commit/a239b4e439be08f1fe2ca730dd82b26febe59161))
* merge_pr keybind 改為 shift-p，避免與 go_to_parent 衝突 ([9d8ad6e](https://github.com/YSzEthan/serie-ys/commit/9d8ad6ea7ab5730cc5f9c31c6bfcafa7507c8416))
* 移除 gh pr merge 不存在的 --yes flag ([38fc84d](https://github.com/YSzEthan/serie-ys/commit/38fc84deb1d6b3b548ee90c196f51372d04de703))
* 補強 merge PR 說明文字（? help 三階段描述、status hint 消歧義） ([44bac7a](https://github.com/YSzEthan/serie-ys/commit/44bac7a9f27f4a819ab290182c053af3058c8a94))
