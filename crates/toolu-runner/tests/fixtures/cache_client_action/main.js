// Mirrors @actions/cache's service selection: v2 (Twirp on ACTIONS_RESULTS_URL)
// only when ACTIONS_CACHE_SERVICE_V2 parses as true, else the retired v1 REST
// API on ACTIONS_CACHE_URL. `__MODE__` (save|restore) and `__FILE__` are filled
// in by the test.
const fs = require('fs');
const mode = '__MODE__';
const file = '__FILE__';
const KEY = 'probe-key';
const VERSION = 'probe-version';
const token = process.env.ACTIONS_RUNTIME_TOKEN;
const auth = { authorization: `Bearer ${token}` };
const v2 = ['true'].includes((process.env.ACTIONS_CACHE_SERVICE_V2 || '').toLowerCase());

async function rpc(method, body) {
  const base = process.env.ACTIONS_RESULTS_URL.replace(/\/?$/, '/');
  const res = await fetch(`${base}twirp/github.actions.results.api.v1.CacheService/${method}`, {
    method: 'POST',
    headers: { ...auth, 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`${method} -> ${res.status}`);
  return res.json();
}

async function main() {
  if (!v2) {
    // Retired v1 path: the test peer serves no such route, so this fails loudly.
    const base = process.env.ACTIONS_CACHE_URL.replace(/\/?$/, '/');
    const res = await fetch(`${base}_apis/artifactcache/cache?keys=${KEY}&version=${VERSION}`, { headers: auth });
    throw new Error(`v1 cache path used (status ${res.status})`);
  }
  if (mode === 'save') {
    const bytes = fs.readFileSync(file);
    const created = await rpc('CreateCacheEntry', { key: KEY, version: VERSION });
    const put = await fetch(created.signed_upload_url, {
      method: 'PUT', headers: { 'x-ms-blob-type': 'BlockBlob' }, body: bytes,
    });
    if (!put.ok) throw new Error(`blob put -> ${put.status}`);
    const done = await rpc('FinalizeCacheEntryUpload', { key: KEY, size_bytes: String(bytes.length), version: VERSION });
    if (!done.ok) throw new Error('finalize not ok');
  } else {
    const hit = await rpc('GetCacheEntryDownloadURL', { key: KEY, restore_keys: [], version: VERSION });
    if (!hit.ok) throw new Error('cache miss');
    const res = await fetch(hit.signed_download_url);
    fs.writeFileSync(file, Buffer.from(await res.arrayBuffer()));
  }
  console.log(`CACHE_${mode.toUpperCase()}_DONE`);
}
main().catch((e) => { console.error('::error::' + e.message); process.exit(1); });
