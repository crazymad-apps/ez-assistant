import { ContentSourceError } from '../dist/site-content/validation.js';
import { publishDirectory } from '../dist/site-content/directory.js';

const [mode, source, output] = process.argv.slice(2);
if (!['build', 'publish'].includes(mode) || !source || !output || process.argv.length !== 5)
  throw new Error('用法：node scripts/site-content.mjs <build|publish> <内容源目录> <公开输出目录>');
try {
  await publishDirectory(source, output, mode);
  process.stdout.write(`静态内容${mode === 'build' ? '已生成' : '已发布'}：${output}\n`);
} catch (error) {
  if (error instanceof ContentSourceError) process.stderr.write(error.message + '\n');
  process.stderr.write('内容处理失败，请检查清单、引用、目录权限与路径；未切换公开内容入口。\n');
  process.exitCode = 1;
}
