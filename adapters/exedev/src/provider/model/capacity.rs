use serde_json::{Value, json};

use super::Spec;

pub(crate) fn capacity_report(spec: &Spec, plan: &Value, vms: &[Value]) -> Value {
    let used_cpus = vms
        .iter()
        .filter_map(|vm| int_field(vm, "allocated_cpus"))
        .sum();
    let used_memory_gb = used_gb(vms, "memory_capacity_bytes");
    let used_disk_gb = used_gb(vms, "disk_capacity_bytes");
    let checks = [
        check("cpus", int_field(plan, "max_cpus"), used_cpus, spec.cpus),
        check(
            "memory_gb",
            int_field(plan, "max_memory_gb"),
            used_memory_gb,
            spec.memory_gb,
        ),
        check(
            "disk_gb",
            int_field(plan, "max_disk_gb"),
            used_disk_gb,
            spec.disk_gb,
        ),
        check("vms", int_field(plan, "max_vms"), vms.len() as u64, 1),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let ok = !checks.is_empty() && checks.iter().all(|row| row["ok"] == true);
    json!({"ok":ok,"status":if ok {"available"} else {"insufficient_capacity"},
        "plan":plan,"checks":checks,
        "usage":{"cpus":used_cpus,"memory_gb":used_memory_gb,"disk_gb":used_disk_gb,"vms":vms.len()},
        "required":{"cpus":spec.cpus,"memory_gb":spec.memory_gb,"disk_gb":spec.disk_gb,"vms":1}})
}

fn used_gb(vms: &[Value], key: &str) -> u64 {
    const GB: u64 = 1024 * 1024 * 1024;
    vms.iter()
        .filter_map(|vm| int_field(vm, key))
        .map(|bytes| bytes.div_ceil(GB))
        .sum()
}

fn check(name: &str, limit: Option<u64>, used: u64, required: u64) -> Option<Value> {
    limit.map(|max| {
        json!({"resource":name,"used":used,"required":required,
            "max":max,"available":max.saturating_sub(used),"ok":used + required <= max})
    })
}

fn int_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64).filter(|n| *n > 0)
}
