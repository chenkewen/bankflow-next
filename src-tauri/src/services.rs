use std::{io::{BufRead, BufReader, Write}, path::Path, process::ChildStdin, sync::{Arc, Mutex}, time::Instant};
use tauri::Emitter;
use crate::{domain::{ConversionEvent, ConversionRequest, TaskItem, TaskSnapshot}, engines};

#[derive(Default)]
struct Task {
    input: Option<ChildStdin>,
    cancelled: bool,
    snapshot: TaskSnapshot,
}

impl Task {
    fn clear(&mut self) -> Result<(), String> {
        if self.snapshot.running {
            return Err("任务仍在处理中，请先取消任务或等待完成后再清除。".into());
        }
        *self = Self::default();
        Ok(())
    }
}

#[derive(Default)]
pub struct ConversionState { task: Arc<Mutex<Task>> }

#[derive(Clone, serde::Serialize)]
struct FinishedEvent<'a> {
    #[serde(flatten)]
    event: &'a ConversionEvent,
    snapshot: &'a TaskSnapshot,
}

fn push_log(snapshot: &mut TaskSnapshot, line: String) {
    if snapshot.logs.len() >= 1000 {
        snapshot.logs.remove(0);
    }
    snapshot.logs.push(line);
}

fn mode_name(mode: &str) -> &str {
    match mode {
        "pdf" => "电子 PDF 转 Excel",
        "ocr" => "OCR 转 Excel",
        "pdf_merge" => "合并 PDF",
        "pdf_split" => "拆分 PDF",
        "pdf_extract" => "提取 PDF 页面",
        "pdf_rotate" => "旋转 PDF",
        "image_convert" => "图片格式转换",
        "images_pdf" => "图片合并 PDF",
        "pdf_images" => "PDF 转图片",
        other => other,
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn finish_item(snapshot: &mut TaskSnapshot, index: usize, event: &ConversionEvent, elapsed: u64, progress: Option<(u32, u32)>) {
    let item = &mut snapshot.items[index];
    let result = match event {
        ConversionEvent::Completed { output } => {
            item.status = "completed".into();
            item.output = Some(output.clone());
            format!("已导出 {output}")
        }
        ConversionEvent::Cancelled { message } => {
            item.status = "cancelled".into();
            item.message = Some(message.clone());
            format!("已取消：{message}")
        }
        ConversionEvent::Error { message } => {
            item.status = "error".into();
            item.message = Some(message.clone());
            format!("失败：{message}")
        }
        _ => return,
    };
    item.elapsed_ms = Some(elapsed);
    let progress = progress.map(|(done, total)| format!("，进度 {done}/{total}")).unwrap_or_default();
    push_log(snapshot, format!("[{}] {result}{progress}，耗时 {elapsed} 毫秒", index + 1));
}

pub fn snapshot(state: &ConversionState) -> Result<TaskSnapshot, String> {
    state.task.lock().map(|task|task.snapshot.clone()).map_err(|_|"任务状态不可用".into())
}

pub fn clear(app: &tauri::AppHandle, state: &ConversionState) -> Result<(), String> {
    let mut task = state.task.lock().map_err(|_| "任务状态不可用")?;
    task.clear()?;
    // 在释放锁之前广播，避免另一窗口启动的新任务先收到开始、随后又收到旧的清除事件。
    let result = app.emit("conversion", ConversionEvent::Cleared { snapshot: task.snapshot.clone() });
    drop(task);
    crate::desktop::update_status(app);
    result.map_err(|error| error.to_string())
}

fn requests(request: ConversionRequest) -> Result<Vec<ConversionRequest>, String> {
    if !matches!(request.mode.as_str(), "pdf" | "ocr" | "pdf_merge" | "pdf_split" | "pdf_extract" | "pdf_rotate" | "image_convert" | "images_pdf" | "pdf_images") {
        return Err("不支持的解析方式".into());
    }
    if !request.separate { return Ok(vec![request]); }
    if !matches!(request.mode.as_str(), "pdf" | "ocr" | "pdf_images" | "pdf_split" | "pdf_rotate") {
        return Err("此操作不支持分别批量转换".into());
    }
    if request.inputs.is_empty() || request.inputs.len()>100 { return Err("批量转换请选择 1–100 个文件".into()); }
    let mut seen=std::collections::HashSet::new();
    request.inputs.iter().map(|input| {
        if !crate::domain::statement_input(std::path::Path::new(input), &request.mode) {
            return Err(if request.mode == "ocr" { "请选择 PDF 或受支持的流水图片" } else { "此批量操作只支持 PDF" }.into());
        }
        if !seen.insert(input.to_lowercase()) {return Err("不能重复添加同一个文件".into());}
        let mut item=request.clone();item.input=input.clone();item.inputs=vec![input.clone()];item.separate=false;Ok(item)
    }).collect()
}

pub fn start(app: tauri::AppHandle, state: &ConversionState, request: ConversionRequest, owner: String) -> Result<(), String> {
    let requests=requests(request)?;
    let mut task=state.task.lock().map_err(|_|"任务状态不可用")?;
    if task.snapshot.running {return Err("已有任务正在处理，请先完成或取消".into());}
    let started = Instant::now();
    task.cancelled=false;
    task.snapshot=TaskSnapshot {running:true, owner, items:requests.iter().map(|request|TaskItem {input:request.input.clone(),status:"pending".into(),..Default::default()}).collect(), logs:vec![format!("开始{}，共 {} 项任务", mode_name(&requests[0].mode), requests.len())], elapsed_ms:None};
    let initial=task.snapshot.clone();
    drop(task);
    crate::desktop::update_status(&app);
    let _=app.emit("conversion",ConversionEvent::Started {snapshot:initial});
    let shared=Arc::clone(&state.task);
    std::thread::spawn(move || {
        let batch=requests.len()>1;
        let mut last=ConversionEvent::Cancelled {message:"任务已取消".into()};
        for (index,request) in requests.into_iter().enumerate() {
            let item_started = Instant::now();
            let mut last_progress = None;
            let launch={
                let mut task=shared.lock().unwrap();
                if task.cancelled {
                    for item in &mut task.snapshot.items[index..] {item.status="skipped".into();item.message=Some("已取消，未执行".into());}
                    let skipped = task.snapshot.items.len() - index;
                    push_log(&mut task.snapshot, format!("任务已取消，剩余 {skipped} 项未执行"));
                    break;
                }
                task.snapshot.items[index].status="running".into();
                let name = Path::new(&request.input).file_name().and_then(|name| name.to_str()).unwrap_or(&request.input);
                push_log(&mut task.snapshot, format!("[{}] 开始处理 {name}", index + 1));
                let _=app.emit("conversion",ConversionEvent::Item {snapshot:task.snapshot.clone()});
                // 启动与登记管道共用锁，取消不会落在两个子任务之间而丢失。
                let launch=if std::path::Path::new(&request.input).is_file() {engines::launch(&request)} else {Err("输入文件不存在".into())};
                match launch {
                    Ok(mut child)=>{task.input=child.stdin.take();Ok(child)}
                    Err(error)=>Err(error),
                }
            };
            last=match launch {
                Err(message)=>ConversionEvent::Error {message},
                Ok(mut child)=>{
                    let log_shared=Arc::clone(&shared);
                    let log_app=app.clone();
                    let secret=request.password.clone();
                    let label=if batch {format!("[{}] ",index+1)} else {String::new()};
                    let stderr=child.stderr.take();
                    let log_reader=std::thread::spawn(move || {
                        if let Some(stderr)=stderr {
                            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                                let line=if secret.is_empty() {line} else {line.replace(&secret,"[已隐藏]")};
                                let line=format!("{}{}",label,line.chars().take(4000).collect::<String>());
                                if let Ok(mut task)=log_shared.lock() {
                                    push_log(&mut task.snapshot, line.clone());
                                }
                                let _=log_app.emit("conversion",ConversionEvent::Log {message:line});
                            }
                        }
                    });
                    let mut terminal=None;
                    if let Some(stdout)=child.stdout.take() {
                        for line in BufReader::new(stdout).lines() {
                            match line.ok().and_then(|line|serde_json::from_str::<ConversionEvent>(&line).ok()) {
                                Some(event @ ConversionEvent::Progress {page, total})=>{
                                    last_progress = Some((page, total));
                                    let _=app.emit("conversion",event);
                                }
                                Some(event @ (ConversionEvent::Completed {..}|ConversionEvent::Cancelled {..}|ConversionEvent::Error {..}))=>terminal=Some(event),
                                _=>{terminal=Some(ConversionEvent::Error {message:"引擎通信异常".into()});let _=child.kill();break;}
                            }
                        }
                    }
                    let exit=child.wait();
                    let _=log_reader.join();
                    if !matches!(exit,Ok(status) if status.success()) && !matches!(terminal,Some(ConversionEvent::Error {..})) {
                        terminal=Some(ConversionEvent::Error {message:"引擎异常退出，未确认转换成功".into()});
                    }
                    terminal.unwrap_or(ConversionEvent::Error {message:"引擎未返回结果".into()})
                }
            };
            let mut task=shared.lock().unwrap();task.input=None;
            if let ConversionEvent::Error { message } = &mut last {
                if !request.password.is_empty() {
                    *message = message.replace(&request.password, "[已隐藏]");
                }
            }
            finish_item(&mut task.snapshot, index, &last, elapsed_ms(item_started), last_progress);
            if matches!(last,ConversionEvent::Cancelled {..}) {task.cancelled=true;}
            let _=app.emit("conversion",ConversionEvent::Item {snapshot:task.snapshot.clone()});
        }
        {
            let mut task = shared.lock().unwrap();
            task.input = None;
            task.snapshot.running = false;
            let elapsed = elapsed_ms(started);
            task.snapshot.elapsed_ms = Some(elapsed);
            if batch {
                push_log(&mut task.snapshot, format!("批量任务结束，总耗时 {elapsed} 毫秒"));
            }
            // 在下一任务可以启动前发布最终快照，保证两窗口收到的结果和耗时属于同一任务。
            if batch {
                let _ = app.emit("conversion", ConversionEvent::Batchcompleted { snapshot: task.snapshot.clone() });
            } else {
                let _ = app.emit("conversion", FinishedEvent { event: &last, snapshot: &task.snapshot });
            }
        }
        crate::desktop::update_status(&app);
    });
    Ok(())
}

pub fn cancel(state: &ConversionState) -> Result<(), String> {
    let mut task=state.task.lock().map_err(|_|"任务状态不可用")?;
    task.cancelled=true;
    if let Some(stdin)=task.input.as_mut() {
        writeln!(stdin,"{{\"cancel\":true}}").map_err(|e|format!("取消请求发送失败：{e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clear_refuses_running_tasks_then_discards_the_finished_record() {
        let mut task = Task {
            snapshot: TaskSnapshot {
                running: true,
                owner: "quick".into(),
                items: vec![TaskItem { input: "流水.pdf".into(), ..Default::default() }],
                logs: vec!["开始处理".into()],
                elapsed_ms: None,
            },
            ..Default::default()
        };
        assert!(task.clear().is_err());
        assert!(task.snapshot.running);
        assert_eq!(task.snapshot.items.len(), 1);
        assert_eq!(task.snapshot.logs.len(), 1);

        task.snapshot.running = false;
        task.snapshot.items[0].output = Some("流水.xlsx".into());
        task.snapshot.elapsed_ms = Some(70);
        task.clear().unwrap();
        task.clear().unwrap();
        let event = ConversionEvent::Cleared { snapshot: task.snapshot };
        let payload = serde_json::to_value(event).unwrap();
        assert_eq!(payload["kind"], "cleared");
        assert_eq!(payload["snapshot"], serde_json::to_value(TaskSnapshot::default()).unwrap());
    }
    fn request()->ConversionRequest {serde_json::from_value(serde_json::json!({"input":"a.pdf","inputs":["a.pdf","b.pdf"],"mode":"pdf_images","password":"","separate":true})).unwrap()}
    #[test]
    fn batch_keeps_each_source_and_settings() {
        let jobs=requests(request()).unwrap();assert_eq!(jobs.len(),2);assert_eq!(jobs[1].input,"b.pdf");assert_eq!(jobs[1].inputs,vec!["b.pdf"]);assert!(!jobs[1].separate);
    }
    #[test]
    fn only_ocr_accepts_images_in_separate_jobs() {
        let mut req = request();
        req.inputs = vec!["扫描.PNG".into(), "流水.webp".into(), "原件.pdf".into()];
        assert!(requests(req.clone()).is_err());
        req.mode = "ocr".into();
        assert_eq!(requests(req.clone()).unwrap().len(), 3);
        req.inputs.push("压缩文件.zip".into());
        assert!(requests(req).is_err());
    }
    #[test]
    fn merge_stays_one_job_and_duplicate_batch_is_rejected() {
        let mut r=request();r.mode="pdf_merge".into();r.separate=false;assert_eq!(requests(r).unwrap().len(),1);
        let mut r=request();r.inputs=vec!["a.pdf".into(),"A.PDF".into()];assert!(requests(r).is_err());
    }

    #[test]
    fn quiet_worker_keeps_log_and_timing_in_terminal_snapshot() {
        let mut snapshot = TaskSnapshot { items: vec![TaskItem::default()], ..Default::default() };
        let event = ConversionEvent::Completed { output: "流水.xlsx".into() };
        finish_item(&mut snapshot, 0, &event, 37, Some((2, 2)));
        snapshot.elapsed_ms = Some(42);
        let result = serde_json::to_value(FinishedEvent { event: &event, snapshot: &snapshot }).unwrap();
        assert_eq!(result["kind"], "completed");
        assert_eq!(result["output"], "流水.xlsx");
        let state = &result["snapshot"];
        assert_eq!(state["elapsed_ms"], 42);
        assert_eq!(state["items"][0]["elapsed_ms"], 37);
        assert!(state["logs"][0].as_str().unwrap().contains("进度 2/2，耗时 37 毫秒"));
    }

    #[test]
    fn failed_or_cancelled_item_keeps_reason_without_timing_unstarted_items() {
        for event in [
            ConversionEvent::Error { message: "无效 PDF".into() },
            ConversionEvent::Cancelled { message: "用户取消".into() },
        ] {
            let mut snapshot = TaskSnapshot { items: vec![TaskItem::default(); 2], ..Default::default() };
            finish_item(&mut snapshot, 0, &event, 0, None);
            assert_eq!(snapshot.items[0].elapsed_ms, Some(0));
            assert_eq!(snapshot.items[1].elapsed_ms, None);
            assert!(snapshot.logs[0].contains(snapshot.items[0].message.as_ref().unwrap()));
        }
    }
}
