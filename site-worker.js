// The site, with byte ranges.
//
// `site/` is a directory of static files and is served as exactly that. This
// Worker sits in front of it for one reason: the asset server answers a
// `Range` request with `200` and the entire body, and advertises no
// `Accept-Ranges` on anything.
//
//     curl -H 'Range: bytes=1000-2000' .../khin-maung-toe.m4a
//     HTTP/2 200    content-length: 6515911
//
// For a media element that is not a slow path, it is a missing capability. A
// browser seeks by asking for the bytes at the new position; without partial
// content the element's `seekable` range stays *empty*, and every assignment
// to `currentTime` reverts to zero however much of the file has been buffered.
// Buffering is not seekability, which is the thing that made this look fixed
// once before when it was not.
//
// So ranges are served here. The asset is fetched whole from the binding —
// which is a cache hit at the edge, not an origin trip — and the requested
// slice is returned with the `206` and the `Content-Range` the standard asks
// for. Everything else is passed through untouched, so a page, a stylesheet
// and a wasm module are served exactly as they were before.

/// Only the things a browser actually seeks in. A `Range` on a stylesheet is
/// not worth the arithmetic, and passing it through keeps this Worker out of
/// the way of every request that is not the one it exists for.
const SEEKABLE = /\.(m4a|mp3|mp4|webm|ogg|opus|wav|flac)$/i;

/// `bytes=start-end`, with either end allowed to be absent: `bytes=500-` is
/// "from here on", and `bytes=-500` is "the last five hundred".
function rangeOf(header, size) {
  const match = /^bytes=(\d*)-(\d*)$/.exec((header || "").trim());
  if (!match) return null;
  const [, rawStart, rawEnd] = match;
  if (rawStart === "" && rawEnd === "") return null;
  let start;
  let end;
  if (rawStart === "") {
    const tail = Number(rawEnd);
    if (!Number.isFinite(tail) || tail <= 0) return null;
    start = Math.max(0, size - tail);
    end = size - 1;
  } else {
    start = Number(rawStart);
    end = rawEnd === "" ? size - 1 : Number(rawEnd);
  }
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  if (start > end || start >= size) return null;
  return { start, end: Math.min(end, size - 1) };
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const asset = await env.ASSETS.fetch(request);

    if (!SEEKABLE.test(url.pathname) || !asset.ok || asset.status !== 200) {
      return asset;
    }

    // Say so even when nothing was asked for: a media element checks
    // `Accept-Ranges` before it decides a resource can be seeked in at all.
    const wanted = request.headers.get("Range");
    if (!wanted) {
      const headers = new Headers(asset.headers);
      headers.set("Accept-Ranges", "bytes");
      return new Response(asset.body, { status: 200, headers });
    }

    const body = await asset.arrayBuffer();
    const span = rangeOf(wanted, body.byteLength);
    if (span === null) {
      // A range that cannot be satisfied has its own status, and the standard
      // asks for the size so the client can ask again sensibly.
      return new Response(null, {
        status: 416,
        headers: {
          "Accept-Ranges": "bytes",
          "Content-Range": `bytes */${body.byteLength}`,
        },
      });
    }

    const headers = new Headers(asset.headers);
    headers.set("Accept-Ranges", "bytes");
    headers.set("Content-Range", `bytes ${span.start}-${span.end}/${body.byteLength}`);
    headers.set("Content-Length", String(span.end - span.start + 1));
    return new Response(body.slice(span.start, span.end + 1), {
      status: 206,
      headers,
    });
  },
};
