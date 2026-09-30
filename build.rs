fn main() {
    // rust-i18n 的 `i18n!` 在編譯期嵌入 locales/，但 cargo 不會自己追蹤這些檔案；
    // 沒有這行，改了翻譯卻不會重編，會吃到舊字串。
    println!("cargo:rerun-if-changed=locales");
}
