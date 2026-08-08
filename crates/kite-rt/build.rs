//! Frame pointers are a correctness requirement, so the build refuses without
//! them.
//!
//! `stack_root_slots` finds GC roots by walking the frame-pointer chain, and on
//! x86-64 Linux and Windows Rust omits frame pointers by default — `rbp` there
//! is an ordinary callee-saved register, and a walk that reads it as a frame
//! record hands the collector a stack word that was never a reference. The
//! workspace's `.cargo/config.toml` passes `-C force-frame-pointers=yes`, but a
//! `RUSTFLAGS` in the environment *replaces* `[build] rustflags` rather than
//! adding to it, so the flag can go missing without anything failing. It went
//! missing in the release workflow exactly that way, and every shipped binary
//! was built without it while `cargo test` was built with it.
//!
//! A convention that only holds when nobody sets an environment variable is not
//! a guarantee. This turns it into a build error.

use std::env;

/// The separator Cargo uses in `CARGO_ENCODED_RUSTFLAGS`.
const SEP: char = '\x1f';

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Apple targets require frame pointers by ABI, AArch64 keeps them for
    // non-leaf frames, and wasm has no frame-pointer chain to walk at all. The
    // hazard is the pair of targets where the register is free for the
    // optimiser to reuse.
    let at_risk = arch == "x86_64" && (os == "linux" || os == "windows");
    if !at_risk {
        return;
    }

    if forced() {
        return;
    }

    panic!(
        "kite-rt on {arch}-{os} needs `-C force-frame-pointers=yes` and it is not set.\n\
         \n\
         The collector walks the frame-pointer chain to find GC roots. Without \
         this flag `rbp` is an ordinary callee-saved register on this target, \
         the walk reads whatever the optimiser left there, and a missed root is \
         a use-after-free.\n\
         \n\
         The workspace sets it in `.cargo/config.toml`. If you are seeing this, \
         something replaced those flags — most likely a `RUSTFLAGS` in the \
         environment, which overrides `[build] rustflags` rather than adding to \
         it. Append `-C force-frame-pointers=yes` to it."
    );
}

/// Whether the flags Cargo will invoke rustc with force frame pointers.
///
/// `CARGO_ENCODED_RUSTFLAGS` is the authority: Cargo assembles it from whichever
/// of its four mutually exclusive sources won, so it reflects the flags actually
/// used rather than the ones any single source asked for.
fn forced() -> bool {
    if let Ok(encoded) = env::var("CARGO_ENCODED_RUSTFLAGS") {
        return says_yes(encoded.split(SEP));
    }
    // Older Cargo, or a direct rustc invocation. Whitespace splitting is wrong
    // for paths with spaces, but this only ever reads a codegen flag's value.
    if let Ok(flags) = env::var("RUSTFLAGS") {
        return says_yes(flags.split_whitespace());
    }
    false
}

/// `-C force-frame-pointers` accepts several spellings, and `-Cfoo` may be
/// written joined or separated, so match on the option rather than the string.
///
/// Every occurrence is read, and any one of them asking for frame pointers
/// decides it. That is rustc's own rule rather than a lenient reading of it:
/// `parse_frame_pointer` *ratchets*, so a later `no` cannot lower an earlier
/// `yes`, and the flags `yes no` compile with frame pointers just as `no yes`
/// does. Returning on the first occurrence agreed with rustc only by luck —
/// it answered `no` to `no yes`, which is a build refused for flags that are
/// in fact safe, and it made this file's own advice unfollowable: the panic
/// says to append `-C force-frame-pointers=yes` to `RUSTFLAGS`, and an
/// appended flag is by definition not the first one.
///
/// `always` and `non-leaf` are accepted because they are the stronger
/// spellings, not unrecognised ones; `non-leaf` suffices because every frame
/// the collector walks has made a call and so is not a leaf.
fn says_yes<'a>(flags: impl Iterator<Item = &'a str>) -> bool {
    let mut expecting_value = false;
    let mut forced = false;
    for flag in flags {
        let opt = if expecting_value {
            expecting_value = false;
            flag
        } else if flag == "-C" {
            expecting_value = true;
            continue;
        } else if let Some(rest) = flag.strip_prefix("-C") {
            rest
        } else {
            continue;
        };

        let Some(rest) = opt.strip_prefix("force-frame-pointers") else {
            continue;
        };
        // Bare `-C force-frame-pointers` means yes; otherwise read the value.
        match rest {
            "" | "=yes" | "=y" | "=on" | "=true" | "=always" | "=non-leaf" => forced = true,
            _ => {}
        }
    }
    forced
}
