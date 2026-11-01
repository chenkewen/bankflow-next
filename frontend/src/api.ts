import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export interface ConversionRequest {
  input: string;
  output_dir: string | null;
  mode: 'pdf' | 'ocr' | 'pdf_merge' | 'pdf_split' | 'pdf_extract' | 'pdf_rotate' | 'image_convert' | 'images_pdf' | 'pdf_images';
  format?: string;
  width?: number;
  quality?: number;
  inputs?: string[];
  pages?: string;
  angle?: number;
  password: string;
  separate?: boolean;
}

export interface TaskItem { input:string; status:'pending'|'running'|'completed'|'error'|'cancelled'|'skipped'; output:string|null; message:string|null }
export interface TaskSnapshot { running:boolean; owner:string; items:TaskItem[] }

export interface ConversionEvent {
  kind: 'progress' | 'completed' | 'cancelled' | 'error' | 'started' | 'item' | 'batchcompleted';
  snapshot?: TaskSnapshot;
  page?: number;
  total?: number;
  output?: string;
  message?: string;
}

export const startConversion = (request: ConversionRequest) =>
  invoke<void>('start_conversion', { request });
export const cancelConversion = () => invoke<void>('cancel_conversion');
export const onConversion = (handler: (event: ConversionEvent) => void) =>
  listen<ConversionEvent>('conversion', event => handler(event.payload));
