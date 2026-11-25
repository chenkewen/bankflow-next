use crate::domain::ConversionRequest;
use std::{
    io::Write,
    process::{Child, Command, Stdio},
};

pub fn launch(request: &ConversionRequest) -> Result<Child, String> {
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command
        .arg("--image-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法启动转换引擎：{e}"))?;
    // 密码通过管道传递，不写入命令行参数或配置文件。
    let payload = serde_json::to_string(request).map_err(|e| e.to_string())?;
    if let Err(error) = writeln!(child.stdin.as_mut().unwrap(), "{payload}") {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("无法提交转换任务：{error}"));
    }
    Ok(child)
}
