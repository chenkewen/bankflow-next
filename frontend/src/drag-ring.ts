import type { DragDropEvent } from '@tauri-apps/api/webview';
import { quickOptions, type QuickOption, type RingProfile } from './quick-options';

import { generalOptions } from './ring-settings';

export function createDragRing(run: (paths:string[], option:QuickOption)=>Promise<void>, available:()=>boolean, report:(message:string)=>void, dismissed:()=>void = ()=>{}) {
  const dialog=document.createElement('dialog'); dialog.className='drag-ring';
  dialog.setAttribute('aria-label','选择快捷转换格式'); document.body.append(dialog);
  let paths:string[]=[], options:QuickOption[]=[];
  let externalDrag=false, consumed=false;
  let profile:RingProfile='bank';
  const choices=(files:string[])=>profile==='general'?generalOptions(files):quickOptions(files);
  const floating=new URLSearchParams(location.search).has('quick');
  let previous:HTMLElement|null=null;
  const close=()=>{dialog.close();if(!floating) previous?.focus();};
  const execute=async(index:number)=>{
    if(consumed || !available() || !options[index]) return;
    consumed=true; externalDrag=false;
    const selected=options[index], files=[...paths]; close(); await run(files,selected);
  };
  function show(files:string[], x=innerWidth/2,y=innerHeight/2, selectedProfile:RingProfile=externalDrag?profile:'bank') {
    if(!available() || document.querySelector('dialog[open]:not(.drag-ring)')) return false;
    profile=selectedProfile;dialog.dataset.profile=profile;
    paths=[...files];options=choices(paths);
    if(!options.length){report('请选择图片，或 PDF；暂不支持混合类型和其他格式。');return false;}
    if(!dialog.open) previous=document.activeElement as HTMLElement;
    consumed=false;
    dialog.replaceChildren();
    const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 336 336');svg.setAttribute('aria-hidden','true');dialog.append(svg);
    const point=(r:number,a:number)=>`${168+r*Math.cos(a)},${168+r*Math.sin(a)}`;
    options.forEach((option,index)=>{
      const a=-Math.PI/2+index*2*Math.PI/options.length+.02,b=-Math.PI/2+(index+1)*2*Math.PI/options.length-.02;
      const sector=document.createElementNS(svg.namespaceURI,'path');
      const large=b-a>Math.PI?1:0;
      sector.setAttribute('d',`M${point(160,a)} A160,160 0 ${large} 1 ${point(160,b)} L${point(72,b)} A72,72 0 ${large} 0 ${point(72,a)} Z`);
      sector.setAttribute('data-sector',String(index));sector.addEventListener('click',()=>void execute(index));svg.append(sector);
      const button=document.createElement('button');button.className='ring-option';button.textContent=option.label;button.dataset.index=String(index);
      button.style.left=`${168+116*Math.cos((a+b)/2)}px`;button.style.top=`${168+116*Math.sin((a+b)/2)}px`;
      button.onclick=()=>void execute(index);button.onfocus=()=>highlight(index);button.onmouseenter=()=>highlight(index);dialog.append(button);
    });
    const center=document.createElement('button');center.className='ring-center';center.textContent=`${profile==='bank'?'银行流水':'通用转换'}\n${paths.length} 个文件 · Esc`;center.onclick=()=>{externalDrag=false;close();};dialog.append(center);
    const hint=document.createElement('p');hint.className='ring-hint';hint.textContent='松到格式上转换 · Esc 或点击外部取消';dialog.append(hint);
    dialog.style.left=`${Math.max(8,Math.min(innerWidth-352,x-168))}px`;
    dialog.style.top=`${Math.max(8,Math.min(innerHeight-390,y-168))}px`;
    if(!dialog.open) { if(externalDrag) dialog.show(); else dialog.showModal(); }
    return true;
  }
  function prepareDrag(selectedProfile:RingProfile='bank') {
    profile=selectedProfile;dialog.dataset.profile=profile;
    if(!available() || document.querySelector('dialog[open]:not(.drag-ring)')) return false;
    externalDrag=true; consumed=false; paths=[]; options=[];
    dialog.replaceChildren();
    const pending=document.createElement('div');pending.className='ring-pending';
    pending.textContent=`${profile==='bank'?'银行流水':'通用转换'}\n拖入图片或 PDF`;dialog.append(pending);
    dialog.style.left='12px';dialog.style.top='10px';
    // 原生拖动不使用模态焦点陷阱，避免争夺资源管理器的拖放焦点。
    if(!dialog.open) dialog.show();
    return true;
  }
  function cancelDrag() {
    if(!externalDrag) return;
    externalDrag=false; close();
  }
  function highlight(index:number) {
    dialog.querySelectorAll('[data-sector]').forEach(el=>el.classList.toggle('active',Number(el.getAttribute('data-sector'))===index));
  }
  function hit(position:{x:number,y:number}) {
    const rect=dialog.getBoundingClientRect();
    const x=position.x/devicePixelRatio-rect.left-168,y=position.y/devicePixelRatio-rect.top-168;
    const radius=Math.hypot(x,y);
    if(!options.length || radius<72 || radius>160) return -1;
    const angle=(Math.atan2(y,x)+Math.PI/2+Math.PI*2)%(Math.PI*2);
    return Math.floor(angle/(Math.PI*2/options.length));
  }
  function handle(event:DragDropEvent) {
    // 只有原生快捷手势会调用 prepareDrag；普通拖放或取消后迟到的 Drop 不能打开圆盘。
    if(!externalDrag) return;
    if(event.type==='enter'){
      if(!choices(event.paths).length){cancelDrag();return;}
      show(event.paths);
    } else if(event.type==='over' && dialog.open) highlight(hit(event.position));
    else if(event.type==='drop'){
      if(!dialog.open || !options.length) {cancelDrag();return;}
      paths=[...event.paths]; options=choices(paths);
      const index=hit(event.position);if(index>=0) void execute(index);else cancelDrag();
    } else if(event.type==='leave') cancelDrag();
  }
  dialog.addEventListener('cancel',event=>{event.preventDefault();externalDrag=false;close();});
  dialog.addEventListener('click',event=>{if(event.target===dialog){externalDrag=false;close();}});
  dialog.addEventListener('close',()=>{if(!floating) previous?.focus();dismissed();});
  document.addEventListener('keydown',event=>{if(event.key==='Escape' && externalDrag) cancelDrag();});
  return { show, prepareDrag, cancelDrag, handle };
}
