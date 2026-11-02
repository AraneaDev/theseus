// CI-only entrypoint for the one fixed MODEL observer. No packages or API client.
const fs = require('node:fs');
const path = require('node:path');
const {createHash} = require('node:crypto');
const {spawnSync} = require('node:child_process');
const pin = 'cf430e030ddbb5b0abf93d22962f4752f3646cd9';
const files = [
  ['action.yml', 3603, 'c5979822866a72362e609844b6ebe77d4b7e759af68cc1c2c425dcf51481fab4'],
  ['package.json', 1832, '6006e753abafb2519b884767c8a67abff6c0c6636e6c83eae1081501fbcda56e'],
  ['dist/upload/index.js', 4452820, 'c0e58b240360573e6d6c82ea1bb22deff4f85883e00211a52bd27822c3a22617'],
];

function verifyFile(file, expected) {
  let fd;
  try { fd = fs.openSync(file, 'r'); } catch { throw new Error('MODEL_UPLOADER_FILE'); }
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile()) throw new Error('MODEL_UPLOADER_FILE');
    if (stat.size !== expected.bytes) throw new Error('MODEL_UPLOADER_SIZE');
    // Read at most one byte past the pinned size, even if the file changes.
    const bytes = Buffer.alloc(expected.bytes + 1);
    let count = 0;
    while (count < bytes.length) {
      const read = fs.readSync(fd, bytes, count, bytes.length - count, null);
      if (read === 0) break;
      count += read;
    }
    if (count !== expected.bytes) throw new Error('MODEL_UPLOADER_SIZE');
    if (createHash('sha256').update(bytes.subarray(0, count)).digest('hex') !== expected.sha256) {
      throw new Error('MODEL_UPLOADER_DIGEST');
    }
  } finally { fs.closeSync(fd); }
}

function validateUploader(temporary) {
  if (typeof temporary !== 'string' || !path.isAbsolute(temporary) ||
      path.basename(temporary).toLowerCase() !== '_temp') {
    throw new Error('MODEL_RUNNER_LAYOUT');
  }
  const action = path.join(path.dirname(temporary), '_actions', 'actions', 'upload-artifact', pin);
  for (const [relative, bytes, sha256] of files) {
    verifyFile(path.join(action, relative), {bytes, sha256});
  }
  return path.join(action, 'dist', 'upload', 'index.js');
}

function validateNode(version) {
  if (!/^24\.[0-9]+\.[0-9]+$/.test(version)) throw new Error('MODEL_NODE_VERSION');
}

function main() {
  if (process.platform !== 'win32') throw new Error('MODEL_WINDOWS_REQUIRED');
  validateNode(process.versions.node);
  const uploader = validateUploader(process.env.RUNNER_TEMP);
  const root = path.resolve(__dirname, '../../..');
  // Only these nonsecret values are added; action runtime values stay inherited.
  process.env.THESEUS_MODEL_NODE = process.execPath;
  process.env.THESEUS_MODEL_UPLOADER = uploader;
  const result = spawnSync('pwsh', ['-NoLogo', '-NoProfile', '-NonInteractive', '-File',
    path.join(root, 'tests', 'support', 'observe_conpty_model.ps1')],
  {cwd: root, shell: false, stdio: 'inherit'});
  // spawnSync has joined the fixed child before this ordinary return.
  if (result.error || result.signal || !Number.isInteger(result.status)) {
    throw new Error('MODEL_OBSERVER_PROCESS');
  }
  process.exitCode = result.status;
}
if (require.main === module) {
  try { main(); } catch {
    // Never print an exception that could contain argv, paths or environment.
    console.error('::error title=Theseus close MODEL action::Fixed observer validation or execution failed');
    process.exitCode = 1;
  }
}
module.exports = {verifyFile, validateUploader, validateNode};
