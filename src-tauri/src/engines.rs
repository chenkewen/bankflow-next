use std::{io::Write, path::PathBuf, process::{Child, Command, Stdio}};
use crate::domain::ConversionRequest;

pub fn launch(request: &ConversionRequest) -> Result<Child, String> {
    // 发布版仅使用随安装包分发的引擎，不依赖开发者源码或用户 PATH。
    let mut command = if cfg!(debug_assertions) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_owned();
        let python = std::env::var_os("BANKFLOW_PYTHON")
            .unwrap_or_else(|| root.join(".venv/Scripts/python.exe").into_os_string());
        let mut cmd = Command::new(python);
        cmd.arg("-u").arg(root.join("scripts/run_worker.py")).current_dir(root);
        cmd
    } else {
        let directory = std::env::current_exe().map_err(|e| e.to_string())?
            .parent().ok_or("无法定位安装目录")?.to_owned();
        Command::new(directory.join("engine/bankflow-worker.exe"))
    };
    command.env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|e| format!("无法启动 Python 引擎，请配置 BANKFLOW_PYTHON：{e}"))?;
    // 密码通过管道传递，不写入命令行参数或配置文件。
    let payload = serde_json::to_string(request).map_err(|e| e.to_string())?;
    if let Err(error) = writeln!(child.stdin.as_mut().unwrap(), "{payload}") {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("无法提交转换任务：{error}"));
    }
    Ok(child)
}
