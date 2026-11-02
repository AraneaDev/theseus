// Integrity refusal controls use real owned files; no native process is mocked.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {test} = require('node:test');
const {verifyFile, validateUploader, validateNode} = require('../../.github/actions/observe-conpty-model/index.cjs');

function owned(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'theseus-model-integrity-'));
  t.after(() => fs.rmSync(root, {recursive: true}));
  return root;
}
const abc = {bytes: 3, sha256: 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'};

test('accepts exact owned bytes without newline conversion', t => {
  const file = path.join(owned(t), 'bytes');
  fs.writeFileSync(file, 'abc');
  assert.equal(verifyFile(file, abc), undefined);
});
test('rejects same-size substituted content', t => {
  const file = path.join(owned(t), 'bytes');
  fs.writeFileSync(file, 'abd');
  assert.throws(() => verifyFile(file, abc), /MODEL_UPLOADER_DIGEST/);
});
test('rejects truncated and oversized files', t => {
  const file = path.join(owned(t), 'bytes');
  for (const bytes of ['ab', 'abcd']) {
    fs.writeFileSync(file, bytes);
    assert.throws(() => verifyFile(file, abc), /MODEL_UPLOADER_SIZE/);
  }
});
test('rejects missing files and directories', t => {
  const root = owned(t);
  assert.throws(() => verifyFile(path.join(root, 'missing'), abc), /MODEL_UPLOADER_FILE/);
  assert.throws(() => verifyFile(root, abc), /MODEL_UPLOADER_FILE/);
});
test('rejects relative or unexpected runner cache layouts', t => {
  const root = owned(t);
  for (const temporary of ['', '_temp', root]) {
    assert.throws(() => validateUploader(temporary), /MODEL_RUNNER_LAYOUT/);
  }
});
test('refuses a missing exact cached action and a substituted manifest', t => {
  const root = owned(t);
  const temporary = path.join(root, '_temp');
  fs.mkdirSync(temporary);
  assert.throws(() => validateUploader(temporary), /MODEL_UPLOADER_FILE/);
  const action = path.join(root, '_actions', 'actions', 'upload-artifact', 'cf430e030ddbb5b0abf93d22962f4752f3646cd9');
  fs.mkdirSync(action, {recursive: true});
  fs.writeFileSync(path.join(action, 'action.yml'), Buffer.alloc(3603));
  assert.throws(() => validateUploader(temporary), /MODEL_UPLOADER_DIGEST/);
});
test('requires the declared Node24 runtime', () => {
  assert.equal(validateNode('24.13.0'), undefined);
  for (const version of ['20.19.0', '26.12.1', '24', '', '24.1.2 injected']) {
    assert.throws(() => validateNode(version), /MODEL_NODE_VERSION/);
  }
});
