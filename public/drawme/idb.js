// A small key-value store in IndexedDB, for the nickname and for the
// drawing, so a reload doesn't lose either. It stores Blobs as they are,
// without encoding them as text, and never blocks the page. Everything here
// fails soft: in a private window, or with site data blocked, the page still
// works and just forgets.

const DATABASE = "drawme";
const STORE = "kv";

/** @type {Promise<IDBDatabase> | undefined} */
let opening;

function open() {
  opening ??= new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  return opening;
}

/**
 * @param {IDBTransactionMode} mode
 * @param {(store: IDBObjectStore) => IDBRequest} use
 */
async function run(mode, use) {
  const db = await open();
  return new Promise((resolve, reject) => {
    const transaction = db.transaction(STORE, mode);
    const request = use(transaction.objectStore(STORE));
    transaction.oncomplete = () => resolve(request.result);
    transaction.onerror = transaction.onabort = () => reject(transaction.error);
  });
}

/** @param {string} key */
export async function get(key) {
  try {
    return await run("readonly", (store) => store.get(key));
  } catch {
    return undefined;
  }
}

/** @param {string} key @param {unknown} value */
export async function set(key, value) {
  try {
    await run("readwrite", (store) => store.put(value, key));
  } catch {
    // Keep going without it.
  }
}
