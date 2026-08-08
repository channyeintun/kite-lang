# Publishing the extension

`RELEASING.md` step 6 is two commands. This is what is behind them, because
the first publish is not the same job as the ones after it, and almost
everything that goes wrong here fails quietly.

## What the Marketplace is, underneath

The Visual Studio Marketplace is a Microsoft property, but the identity it
runs on is **Azure DevOps**. That is why a `vsce` command answers a bad token
with `TF400813`, an Azure DevOps error code, in a tool that never mentions
Azure. Two accounts are involved and it is worth keeping them apart:

- a **Microsoft account** — what you sign in to the Marketplace with, and all
  the *web* route ever needs;
- an **Azure DevOps organisation** — free, at `dev.azure.com`, and needed only
  to mint the Personal Access Token the *command line* route uses.

Neither is an Azure cloud subscription. If a signup asks for a credit card you
are on `portal.azure.com`, which is the paid product; the free organisation is
created from `dev.azure.com`, and the Marketplace docs link to a page that can
route to either.

## The publisher, once, before anything

An extension is published *by* a publisher, and `package.json` names ours:

```json
"publisher": "kite-lang"
```

The publisher must already exist, and its **ID cannot be changed afterwards**,
so it has to match that line exactly. It is created in the browser — there is
no CLI for it:

<https://marketplace.visualstudio.com/manage/createpublisher>

Ours is ID `kite-lang`, display name `Kite`. The rest of the form is the
public profile page and is editable later: description, logo, website,
support link, source repository.

Two things about that form cost an attempt:

**It fails silently.** Clicking *Create* with an invalid field does nothing at
all — no message near the button, no scroll to the problem. The error sits
beside the offending field, which may be several screens up. The tell is that
the browser then asks "Leave site?" when you navigate away, because the form
still holds unsaved changes. If *Create* appears to do nothing, scroll up and
look for red text rather than clicking it again.

**The domain is not the website.** *Verified domain* is an ownership claim
that wants a DNS record, and an unverified one is fine to leave empty; the
*Company website* field further down is the ordinary link. Putting a URL in
the first when you meant the second is how the invalid-field state above
happens.

## Publishing

### The web route, which needs no token

*Publisher → New extension → Visual Studio Code*, and drop in a `.vsix` built
by:

```bash
cd editors/vscode && npx @vscode/vsce package
```

This is the whole of it. For a first publish it is the better route, because
publisher-setup problems surface in the interface rather than as an error code
from a token exchange.

### The command line route, which needs one

The token is an **Azure DevOps Personal Access Token**, and two of its fields
matter:

- **Scopes** — `Marketplace` → `Manage`.
- **Organization** — `All accessible organizations`. Not one of them.

Selecting a single organisation is the common mistake and it produces
`TF400813: The user '…' is not authorized to access this resource`, which
reads like a permissions problem with your account rather than a dropdown you
chose wrong. A token with no publisher rights at all shows the user as
`aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa`, which is `vsce`'s way of saying it
authenticated as nobody.

```bash
npx @vscode/vsce login kite-lang     # paste the PAT
npx @vscode/vsce publish             # from editors/vscode
```

`login` verifies the token *against that publisher*, so the publisher has to
exist first — the login fails even with a perfectly good token otherwise.

## The version

`vsce publish` publishes whatever `package.json` says, and the Marketplace
refuses a version it already has. The number is set by the release, not here:
`RELEASING.md` step 2 moves it in five places at once, and
`every_version_stays_on_the_one_line` fails the build if they disagree. Do not
bump it in this directory alone.

## The icon

The Marketplace will not take an SVG, so `icon.png` is a rendering that has to
exist as a file. `render-icon.sh` regenerates it, and it is only needed when
the mark itself changes — `brand_assets.rs` fails the test suite if the PNG is
missing or has drifted from `site/kite-mark.svg`, which is what keeps the two
from separating quietly.

The publisher's own logo is a different image, set on the publisher's *Details*
tab, and the form asks for 128×128 where `icon.png` is 256×256.

## Afterwards

A freshly published extension takes a few minutes to appear, and the
Marketplace serves a stale page for a while after that. Check the item itself
rather than the search results, which lag further:

```bash
npx @vscode/vsce show kite-lang.kite-lang
```

`Extension "kite-lang.kite-lang" not found` immediately after publishing is
propagation. The same message an hour later is not.
