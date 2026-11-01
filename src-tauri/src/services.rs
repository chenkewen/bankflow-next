use std::{io::{BufRead, BufReader, Write}, process::ChildStdin, sync::{Arc, Mutex}};
use tauri::Emitter;
use crate::{domain::{ConversionEvent, ConversionRequest, TaskItem, TaskSnapshot}, engines};

#[derive(Default)]
struct Task {
    input: Option<ChildStdin>,
    cancelled: bool,
    snapshot: TaskSnapshot,
}
#[derive(Default)]
pub struct ConversionState { task: Arc<Mutex<Task>> }

pub fn snapshot(state: &ConversionState) -> Result<TaskSnapshot, String> {
    state.task.lock().map(|task|task.snapshot.clone()).map_err(|_|"任务状态不可用".into())
}

fn requests(request: ConversionRequest) -> Result<Vec<ConversionRequest>, String> {
    if !matches!(request.mode.as_str(), "pdf" | "ocr" | "pdf_merge" | "pdf_split" | "pdf_extract" | "pdf_rotate" | "image_convert" | "images_pdf" | "pdf_images") {
        return Err("不支持的解析方式".into());
    }
    if !request.separate { return Ok(vec![request]); }
    if !matches!(request.mode.as_str(), "pdf" | "ocr" | "pdf_images" | "pdf_split" | "pdf_rotate") {
        return Err("此操作不支持分别批量转换".into());
    }
    if request.inputs.is_empty() || request.inputs.len()>100 { return Err("批量转换请选择 1–100 份 PDF".into()); }
    let mut seen=std::collections::HashSet::new();
    request.inputs.iter().map(|input| {
        if !input.to_lowercase().ends_with(".pdf") {return Err("分别批量转换目前只支持 PDF".into());}
        if !seen.insert(input.to_lowercase()) {return Err("不能重复添加同一个文件".into());}
        let mut item=request.clone();item.input=input.clone();item.inputs=vec![input.clone()];item.separate=false;Ok(item)
    }).collect()
}

pub fn start(app: tauri::AppHandle, state: &ConversionState, request: ConversionRequest, owner: String) -> Result<(), String> {
    let requests=requests(request)?;
    let mut task=state.task.lock().map_err(|_|"任务状态不可用")?;
    if task.snapshot.running {return Err("已有任务正在处理，请先完成或取消".into());}
    task.cancelled=false;
    task.snapshot=TaskSnapshot {running:true, owner, items:requests.iter().map(|request|TaskItem {input:request.input.clone(),status:"pending".into(),..Default::default()}).collect()};
    let initial=task.snapshot.clone();
    drop(task);
    crate::desktop::update_status(&app,&initial);
    let _=app.emit("conversion",ConversionEvent::Started {snapshot:initial});
    let shared=Arc::clone(&state.task);
    std::thread::spawn(move || {
        let batch=requests.len()>1;
        let mut last=ConversionEvent::Cancelled {message:"任务已取消".into()};
        for (index,request) in requests.into_iter().enumerate() {
            let launch={
                let mut task=shared.lock().unwrap();
                if task.cancelled {
                    for item in &mut task.snapshot.items[index..] {item.status="skipped".into();item.message=Some("已取消，未执行".into());}
                    break;
                }
                task.snapshot.items[index].status="running".into();
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
                    let mut terminal=None;
                    if let Some(stdout)=child.stdout.take() {
                        for line in BufReader::new(stdout).lines() {
                            match line.ok().and_then(|line|serde_json::from_str::<ConversionEvent>(&line).ok()) {
                                Some(event @ ConversionEvent::Progress {..})=>{let _=app.emit("conversion",event);}
                                Some(event @ (ConversionEvent::Completed {..}|ConversionEvent::Cancelled {..}|ConversionEvent::Error {..}))=>terminal=Some(event),
                                _=>{terminal=Some(ConversionEvent::Error {message:"引擎通信异常".into()});let _=child.kill();break;}
                            }
                        }
                    }
                    let exit=child.wait();
                    if !matches!(exit,Ok(status) if status.success()) && !matches!(terminal,Some(ConversionEvent::Error {..})) {
                        terminal=Some(ConversionEvent::Error {message:"引擎异常退出，未确认转换成功".into()});
                    }
                    terminal.unwrap_or(ConversionEvent::Error {message:"引擎未返回结果".into()})
                }
            };
            let mut task=shared.lock().unwrap();task.input=None;
            let item=&mut task.snapshot.items[index];
            match &last {
                ConversionEvent::Completed {output}=>{item.status="completed".into();item.output=Some(output.clone());}
                ConversionEvent::Cancelled {message}=>{item.status="cancelled".into();item.message=Some(message.clone());}
                ConversionEvent::Error {message}=>{item.status="error".into();item.message=Some(message.clone());}
                _=>{}
            }
            if matches!(last,ConversionEvent::Cancelled {..}) {task.cancelled=true;}
            let _=app.emit("conversion",ConversionEvent::Item {snapshot:task.snapshot.clone()});
        }
        let snapshot={let mut task=shared.lock().unwrap();task.input=None;task.snapshot.running=false;task.snapshot.clone()};
        crate::desktop::update_status(&app,&snapshot);
        // 整批任务释放后才发送结束事件，下一批不会被旧任务的清理覆盖。
        let event=if batch {ConversionEvent::Batchcompleted {snapshot}} else {last};
        let _=app.emit("conversion",event);
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
    fn request()->ConversionRequest {serde_json::from_value(serde_json::json!({"input":"a.pdf","inputs":["a.pdf","b.pdf"],"mode":"pdf_images","password":"","separate":true})).unwrap()}
    #[test]
    fn batch_keeps_each_source_and_settings() {
        let jobs=requests(request()).unwrap();assert_eq!(jobs.len(),2);assert_eq!(jobs[1].input,"b.pdf");assert_eq!(jobs[1].inputs,vec!["b.pdf"]);assert!(!jobs[1].separate);
    }
    #[test]
    fn merge_stays_one_job_and_duplicate_batch_is_rejected() {
        let mut r=request();r.mode="pdf_merge".into();r.separate=false;assert_eq!(requests(r).unwrap().len(),1);
        let mut r=request();r.inputs=vec!["a.pdf".into(),"A.PDF".into()];assert!(requests(r).is_err());
    }
}
