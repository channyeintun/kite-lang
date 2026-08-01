// The playground: the compiler itself, compiled to WebAssembly.
//
// A language whose site cannot run the language is asking to be taken on
// faith. Nothing here talks to a server — the module below is `kitec`, and the
// diagnostics it produces are the diagnostics a terminal shows, because they
// are produced by the same code.

let wasm;
let memory;

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export async function load(url = "kite_playground.wasm") {
  const { instance } = await WebAssembly.instantiateStreaming(fetch(url), {}).catch(
    async () => {
      // `instantiateStreaming` needs the right content type, which a plain
      // file server does not always send.
      const bytes = await (await fetch(url)).arrayBuffer();
      return WebAssembly.instantiate(bytes, {});
    },
  );
  wasm = instance.exports;
  memory = wasm.memory;
  return wasm;
}

/// Hand a string to the module, run `name`, and read the answer back.
function call(name, source, extra) {
  const bytes = encoder.encode(source);
  const ptr = wasm.kite_alloc(bytes.length);
  new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);

  let answer;
  if (extra === undefined) {
    answer = wasm[name](ptr, bytes.length);
  } else {
    const extraBytes = encoder.encode(extra);
    const extraPtr = wasm.kite_alloc(extraBytes.length);
    new Uint8Array(memory.buffer, extraPtr, extraBytes.length).set(extraBytes);
    answer = wasm[name](ptr, bytes.length, extraPtr, extraBytes.length);
    wasm.kite_free(extraPtr, extraBytes.length);
  }

  const length = wasm.kite_answer_length();
  const text = decoder.decode(new Uint8Array(memory.buffer, answer, length));
  wasm.kite_free(answer, length);
  wasm.kite_free(ptr, bytes.length);
  return text;
}

export const run = (source) => call("kite_run", source);
export const check = (source) => call("kite_check", source);
export const format = (source) => call("kite_format", source);
export const docs = (source) => call("kite_docs", source);
export const emit = (source, stage) => call("kite_emit", source, stage);
