// マニフェストは埋め込まない。DPI は実行時 `SetProcessDpiAwarenessContext` で設定する。
// アプリケーションアイコンだけを MSVC バイナリへ入れる。AppUserModelID は付けない。

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows-msvc") {
        embed_resource::compile("assets/app.rc", embed_resource::NONE);
    }
}
