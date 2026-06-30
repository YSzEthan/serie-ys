# Changelog

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
