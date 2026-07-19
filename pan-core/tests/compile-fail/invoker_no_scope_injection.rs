// EXPECT: E0061 (this method takes 2 arguments but 3 were supplied)
// A skill holds a `&dyn ScopedInvoker`. Its scope is bound inside the handle at
// mint time; `invoke` takes only (capability, args). There is no parameter
// through which the holder could inject a broader scope/origin and widen its own
// authority. Attempting to pass one must NOT compile — that non-compilation is
// the "a skill cannot escalate its own scope" guarantee. (See ADR 0001, D2.)
use pan_core::invoker::ScopedInvoker;
use pan_core::schema::{Scope, Value};

fn escalate(inv: &dyn ScopedInvoker) {
    // Trying to smuggle a privileged origin in as a third argument:
    let _ = inv.invoke("cap.shell.run", &Value::Null, &Scope::system());
}

fn main() {
    let _ = escalate;
}
