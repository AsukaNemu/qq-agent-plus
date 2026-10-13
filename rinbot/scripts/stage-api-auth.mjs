// Copy the existing console credential into an unloaded, repository-local
// RInBot plist. Credentials are passed to plutil on stdin and never printed.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';

const [sourceConfig, plistFile, ...extra] = process.argv.slice(2);
if (!sourceConfig || !plistFile || extra.length || !path.isAbsolute(sourceConfig) || !path.isAbsolute(plistFile)) {
  throw new Error('Usage: node stage-api-auth.mjs <absolute Node config.json> <absolute staged RInBot plist>');
}
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const stageRoot = fs.realpathSync(path.join(repo, '_staging'));
const target = fs.realpathSync(plistFile);
if (!target.startsWith(`${stageRoot}${path.sep}`) || fs.lstatSync(plistFile).isSymbolicLink()) {
  throw new Error('The target must be an unloaded, ordinary plist inside this repository staging directory');
}
for (const file of [sourceConfig, target]) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size > 4 * 1024 * 1024) throw new Error('Configuration must be a bounded ordinary file');
}
const plist = JSON.parse(execFileSync('/usr/bin/plutil', ['-convert', 'json', '-o', '-', target], { encoding: 'utf8' }));
if (plist.Label !== 'com.local.rinbot' || plist.ProgramArguments?.some(value => value === '--api-token' || value.startsWith('--api-token='))) {
  throw new Error('The staged plist must belong to RInBot and use an environment credential');
}
const dataIndex = plist.ProgramArguments.indexOf('--data-dir');
if (dataIndex < 0 || !fs.realpathSync(plist.ProgramArguments[dataIndex + 1]).startsWith(`${stageRoot}${path.sep}`)) {
  throw new Error('The staged data directory must stay inside this repository staging directory');
}
const sourceToken = JSON.parse(fs.readFileSync(sourceConfig, 'utf8')).server?.token;
const generated = !sourceToken;
const token = sourceToken || plist.EnvironmentVariables?.QQBOTD_API_TOKEN || randomBytes(32).toString('hex');
if (typeof token !== 'string' || !token || token.length > 8192 || /[\x00-\x1f\x7f]/.test(token)) {
  throw new Error('The console credential is invalid');
}
const accessFile = path.join(path.dirname(target), 'console-access.txt');
const accessText = `RInBot Console: http://127.0.0.1:3211/\nToken: ${token}\n`;
if (fs.existsSync(accessFile) && (!fs.lstatSync(accessFile).isFile() || fs.readFileSync(accessFile, 'utf8') !== accessText)) {
  throw new Error('A different staged credential file exists; preserve it before changing credentials');
}
plist.EnvironmentVariables = { ...plist.EnvironmentVariables, QQBOTD_API_TOKEN: token };
const xml = execFileSync('/usr/bin/plutil', ['-convert', 'xml1', '-o', '-', '-'], { input: JSON.stringify(plist) });
const temporary = `${target}.${process.pid}.tmp`;
const descriptor = fs.openSync(temporary, 'wx', 0o600);
try {
  fs.writeFileSync(descriptor, xml);
  fs.fsyncSync(descriptor);
} finally {
  fs.closeSync(descriptor);
}
fs.renameSync(temporary, target);
if (!fs.existsSync(accessFile)) fs.writeFileSync(accessFile, accessText, { flag: 'wx', mode: 0o600 });
console.log(`API_AUTH_STAGED=true; credential=${generated ? 'private staged token' : 'existing Node token'}; no service was loaded or restarted`);
