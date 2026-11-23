//! A web program run on the bytecode VM, which has no JavaScript.
//!
//! `kitec run` checks a program before lowering it, and a program written for
//! the web is valid Kite, so it is not refused; it stops at its first host
//! call instead. `js.func` was lowered as `task.spawn`, so the VM ran the
//! closure meant for a JavaScript event as a task, printing whatever it
//! printed, and then trapped saying a task had been handed a `()`.

use std::process::Command;

const KITEC: &str = env!("CARGO_BIN_EXE_kitec");

#[test]
fn js_func_on_the_vm_traps_before_the_closure_runs() {
    let dir = std::env::temp_dir().join(format!("kitec-no-host-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a work directory");
    let program = dir.join("web.kite");
    std::fs::write(
        &program,
        "fn main() {\n    io.print(\"before\")\n    let f = js.func(|| { io.print(\"closure body ran\") })\n    io.print(\"made\")\n}\n",
    )
    .expect("write the program");
    let out = Command::new(KITEC).arg("run").arg(&program).output().expect("kitec runs");
    let (stdout, stderr) =
        (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "{}{}", stdout, stderr);
    assert_eq!(stdout, "before\n", "{}", stderr);
    assert!(
        stderr.contains("`js.func` is a host function, and this runtime supplies no host"),
        "{}",
        stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}
