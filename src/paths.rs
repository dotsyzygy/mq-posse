//! Evaluating the `Me` paths other clients ask this one for.

use std::time::{Duration, Instant};

use mq_emu::{MACROQUEST_MEMBERS, MACROQUEST_METHODS, Mq, Value};
use posse_core::{Evaluator, PathValue, Region, SlotId, parse_path};
use posse_shm::now_ms;

use crate::Posse;

/// Time spent evaluating paths per pulse. The first one due is always
/// evaluated, as each is evaluated no more often than every 10 times its cost.
const BUDGET: Duration = Duration::from_millis(1);

fn listed(names: &[&str], name: &str) -> bool {
    names.iter().any(|listed| listed.eq_ignore_ascii_case(name))
}

/// A name that's a method on every type that has it, which would act rather
/// than read (`Sit`, `DoTarget`, `Remove`).
pub fn only_a_method(name: &str) -> bool {
    listed(MACROQUEST_METHODS, name) && !listed(MACROQUEST_MEMBERS, name)
}

/// Whether reading `name` from a value of type `type_name` only reads. A name
/// that's a member of some types and a method of others (`Target`) is read
/// only from a type that has it as a member.
fn reads(mq: &Mq<Posse>, type_name: &str, name: &str) -> bool {
    if !listed(MACROQUEST_METHODS, name) {
        return true;
    }
    !only_a_method(name) && mq.data(&format!("Type[{type_name}].Member[{name}]")).ok().flatten().is_some()
}

/// `${Me.<path>}`, refusing a path that's invalid or would call a method.
pub fn evaluate(mq: &Mq<Posse>, path: &str) -> PathValue {
    let Ok(steps) = parse_path(path) else { return PathValue::Null };
    let Some(mut data) = mq.tlo("Me") else { return PathValue::Null };
    for step in &steps {
        if !reads(mq, &data.type_name(), &step.name) {
            return PathValue::Null;
        }
        let Some(next) = data.member_at(&step.name, &step.index) else { return PathValue::Null };
        data = next;
    }
    match data.value() {
        Value::Bool(b) => PathValue::Bool(b),
        Value::Int(n) => PathValue::Int(n),
        Value::Float(x) => PathValue::Float(x),
        Value::String(text) => PathValue::String(text),
        Value::Object { text, .. } => PathValue::Object(text),
        _ => PathValue::Object(data.text()),
    }
}

/// Evaluates the paths due, until the pulse's budget is spent.
pub fn evaluate_due(mq: &Mq<Posse>, evaluator: &mut Evaluator, region: &Region, me: SlotId) {
    let started = Instant::now();
    for path in evaluator.due(now_ms()) {
        if started.elapsed() >= BUDGET {
            break;
        }
        let began = Instant::now();
        let value = evaluate(mq, &path);
        evaluator.record(region, me, &path, value, now_ms(), began.elapsed());
    }
}

/// A step of a path as written: `Buff[3]`, or `Target` without an index.
pub fn step(name: &str, index: &str) -> String {
    if index.is_empty() { name.to_owned() } else { format!("{name}[{index}]") }
}
