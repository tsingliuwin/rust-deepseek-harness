//! Windows 资源嵌入：exe/任务栏图标 + 版本信息。
//! gpui 的 windows 平台在建窗口时从本模块资源 ID 1 LoadImageW
//! 取图标（见 gpui-0.2.2 platform/windows/platform.rs load_icon），
//! 无资源时回落系统默认灰窗——必须嵌为 ID 1（winres set_icon 即此）。

fn main() {
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "鲸像 WhaleMirror");
        res.set("FileDescription", "鲸像 WhaleMirror");
        res.compile().expect("embed windows resources");
    }
}
