use crate::{
    domain::{ConversionEvent, ConversionRequest},
    engines,
};
use std::{
    error::Error,
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    thread,
};

const USAGE:&str="用法：bankflow-workbench --convert {pdf|ocr} 文件或目录 [-o 输出目录] [-r] [-p 密码]\n  -o, --output     输出目录（默认在输入文件旁）\n  -r, --recursive  递归扫描子目录\n  -p, --password   PDF 打开密码";
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Options {
    mode: String,
    input: PathBuf,
    output: Option<PathBuf>,
    password: String,
    recursive: bool,
}

fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Option<Options>> {
    use lexopt::prelude::*;
    let mut parser = lexopt::Parser::from_args(args);
    let mut mode = None;
    let mut input = None;
    let mut output = None;
    let mut password = String::new();
    let mut recursive = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(None),
            Value(s) if mode.is_none() => {
                let s = s.into_string().map_err(|_| "模式必须为 pdf 或 ocr")?;
                if s != "pdf" && s != "ocr" {
                    return Err("模式必须为 pdf 或 ocr".into());
                }
                mode = Some(s);
            }
            Value(s) if input.is_none() => input = Some(PathBuf::from(s)),
            Short('o') | Long("output") => output = Some(PathBuf::from(parser.value()?)),
            Short('p') | Long("password") => {
                password = parser
                    .value()?
                    .into_string()
                    .map_err(|_| "密码须为有效文本")?
            }
            Short('r') | Long("recursive") => recursive = true,
            _ => return Err(arg.unexpected().into()),
        }
    }
    Ok(Some(Options {
        mode: mode.ok_or("请指定 pdf 或 ocr")?,
        input: input.ok_or("请指定文件或目录")?,
        output,
        password,
        recursive,
    }))
}

fn inputs(opt: &Options) -> Result<Vec<PathBuf>> {
    if opt.input.is_file() {
        return Ok(vec![opt.input.clone()]);
    }
    if !opt.input.is_dir() {
        return Err(format!("路径不存在：{}", opt.input.display()).into());
    }
    let mut files = Vec::new();
    let mut pending = vec![opt.input.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() && opt.recursive {
                pending.push(path);
                continue;
            }
            if !path.is_file() {
                continue;
            }
            if crate::domain::statement_input(&path, &opt.mode) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn one(file: &Path, opt: &Options) -> Result<()> {
    let file = std::path::absolute(file)?;
    let dir = opt
        .output
        .as_deref()
        .unwrap_or_else(|| file.parent().unwrap());
    fs::create_dir_all(dir)?;
    // 单文件完成后原子替换指定结果。目录任务直接复用 worker 的无覆盖发布。
    let stage = if opt.input.is_file() {
        Some(tempfile::Builder::new().prefix("bankflow-cli-").tempdir_in(dir)?)
    } else {
        None
    };
    let work_dir = stage.as_ref().map_or(dir, |temp| temp.path());
    // 显式输入的文件可以没有 PDF 后缀，旧 CLI 会按 PDF 内容尝试解析。
    let mut input = file.clone();
    if !crate::domain::statement_input(&file, &opt.mode) {
        input = work_dir.join("input.pdf");
        if fs::hard_link(&file, &input).is_err() {
            fs::copy(&file, &input)?;
        }
    }
    let req = ConversionRequest {
        mode: opt.mode.clone(),
        input: input.to_str().ok_or("文件名不是有效文本")?.into(),
        output_dir: Some(
            std::path::absolute(work_dir)?
                .to_string_lossy()
                .into_owned(),
        ),
        password: opt.password.clone(),
        inputs: vec![],
        pages: String::new(),
        angle: 90,
        format: "png".into(),
        width: 0,
        quality: 90,
        separate: false,
    };
    let mut child = engines::launch(&req)?;
    let mut log = child.stderr.take().unwrap();
    let logger = thread::spawn(move || io::copy(&mut log, &mut io::stderr().lock()));
    let mut output = None;
    let mut error = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        match line
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<ConversionEvent>(&s).map_err(|e| e.to_string()))
        {
            Ok(ConversionEvent::Completed { output: path }) => output = Some(path),
            Ok(ConversionEvent::Error { message } | ConversionEvent::Cancelled { message }) => {
                error = Some(message)
            }
            Ok(_) => (),
            Err(e) => error = Some(e),
        }
    }
    let status = child.wait()?;
    logger.join().map_err(|_| "无法读取引擎日志")??;
    if let Some(msg) = error {
        return Err(msg.into());
    }
    if !status.success() {
        return Err(format!("转换引擎异常退出（{status}）").into());
    }
    let output = output.ok_or("转换引擎未生成 Excel")?;
    let dest = if stage.is_some() {
        let dest = dir.join(file.file_name().ok_or("文件名无效")?).with_extension("xlsx");
        if dest.exists() && fs::canonicalize(&dest)? == fs::canonicalize(&file)? {
            return Err("输出与输入指向同一文件，请指定其他输出目录".into());
        }
        fs::rename(output, &dest)?;
        dest
    } else {
        PathBuf::from(output)
    };
    println!("{}", dest.display());
    Ok(())
}

pub fn run() -> i32 {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        // GUI 子系统的安装版在命令行模式连接父终端，保留管道重定向。
        unsafe {
            let _ = windows::Win32::System::Console::AttachConsole(
                windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    let opt = match parse(std::env::args_os().skip(2)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let files = match inputs(&opt) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut failed = 0;
    for file in &files {
        if let Err(e) = one(file, &opt) {
            eprintln!("处理失败 {}：{e}", file.display());
            failed += 1;
        }
    }
    if opt.input.is_dir() {
        eprintln!("批量完成：成功 {}，失败 {failed}", files.len() - failed);
    }
    i32::from(failed != 0)
}
