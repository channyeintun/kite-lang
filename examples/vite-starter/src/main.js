// The JavaScript, which is the smaller half on purpose.
//
// It reads the page, hands values to Kite, and puts the answers back. Every
// decision that could be wrong — what a line costs, how tax rounds, how money
// is written, whether a card number is a typo — is on the other side of this
// import.
import {
  load, line_total, tax, discount, money, card_looks_valid, parse_price,
} from "./checkout.kite";

// One call before anything else: it fetches and instantiates the module. The
// plugin gave `load` the URL Vite chose, so this is the same in dev and in a
// build.
await load();

// `int` is 64-bit in Kite, so it crosses as a BigInt. That is the one thing a
// caller has to know, and `api.d.ts` says so where an editor can show it.
const VAT_BASIS_POINTS = 2000n; // 20%

const lines = [
  { what: "Keyboard", price: 8999n, quantity: 1n },
  { what: "Cable", price: 450n, quantity: 3n },
  { what: "Stand", price: 2075n, quantity: 1n },
];

const el = (id) => document.getElementById(id);

function render() {
  el("items").replaceChildren(
    ...lines.map(({ what, price, quantity }) => {
      const li = document.createElement("li");
      li.innerHTML =
        `<span>${what} <small>× ${quantity}</small></span>` +
        `<output>${money(line_total(price, quantity))}</output>`;
      return li;
    }),
  );

  const subtotal = lines.reduce((n, l) => n + line_total(l.price, l.quantity), 0n);
  const off = discount(subtotal, BigInt(el("percent").value || 0));
  const net = subtotal - off;
  const vat = tax(net, VAT_BASIS_POINTS);

  el("subtotal").textContent = money(subtotal);
  el("discount").textContent = "−" + money(off);
  el("vat").textContent = money(vat);
  el("total").textContent = money(net + vat);
}

el("percent").addEventListener("input", render);

el("add").addEventListener("click", () => {
  // `parse_price` answers -1 rather than an optional, because an `Option<int>`
  // does not cross the wrapper yet. Inside Kite the caller could not forget to
  // open it; here, forgetting is possible and this is the check.
  const price = parse_price(el("price").value);
  const note = el("price-note");
  if (price < 0n) {
    note.textContent = "that is not a price";
    note.classList.add("failed");
    return;
  }
  note.textContent = "";
  note.classList.remove("failed");
  lines.push({ what: el("what").value || "Something", price, quantity: 1n });
  render();
});

const card = el("card");
card.addEventListener("input", () => {
  const verdict = el("card-note");
  if (card.value.trim() === "") {
    verdict.textContent = "";
    verdict.className = "note";
    return;
  }
  const ok = card_looks_valid(card.value);
  verdict.textContent = ok ? "passes the Luhn check" : "does not check out";
  verdict.className = ok ? "note passes" : "note failed";
});

render();
