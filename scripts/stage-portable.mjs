import { copyFile, cp, mkdir, rename, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const release = resolve(process.env.CARGO_TARGET_DIR || 'src-tauri/target', 'release');
const portable = resolve('build', 'portable');
const executable = 'bankflow-workbench.exe';
const staged = join(portable, `.${executable}.${process.pid}.tmp`);

await mkdir(portable, { recursive: true });
try {
  // 先备齐模型和 DLL，再发布可执行文件，避免双击时读到尚未复制完整的 EXE。
  await copyFile(join(release, executable), staged);
  await cp(join(release, 'runtime'), join(portable, 'runtime'), { recursive: true });
  await rename(staged, join(portable, executable));
} finally {
  await rm(staged, { force: true });
}
console.log(join(portable, executable));
