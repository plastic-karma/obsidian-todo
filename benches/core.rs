use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use obsidian_todo::frontmatter::{parse_task, serialize_task};
use obsidian_todo::recurrence::parse_date;
use obsidian_todo::{initialize, Config, InitOptions, RecurrenceMode, RecurrenceRule, Store, Task};
use serde_yaml_ng::Mapping;
use tempfile::TempDir;
use ulid::Ulid;

fn date(value: &str) -> NaiveDate {
    parse_date(value, "date").expect("benchmark date")
}

fn recurrence_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("recurrence_next_due");
    group.sample_size(100);
    let due = date("2020-01-06");
    let far_late = date("2099-12-31");
    let weekly =
        RecurrenceRule::parse("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR").expect("weekly rule");
    group.bench_function("weekly_far_late_schedule", |bencher| {
        bencher.iter(|| {
            weekly
                .next_due(
                    black_box(due),
                    black_box(far_late),
                    RecurrenceMode::Schedule,
                )
                .expect("next weekly date")
        });
    });

    let monthly = RecurrenceRule::parse("FREQ=MONTHLY;BYMONTHDAY=29,30,31").expect("monthly rule");
    group.bench_function("monthly_sparse_days", |bencher| {
        bencher.iter(|| {
            monthly
                .next_due(
                    black_box(date("2026-01-31")),
                    black_box(date("2046-02-28")),
                    RecurrenceMode::Schedule,
                )
                .expect("next monthly date")
        });
    });
    group.finish();
}

fn frontmatter_benchmarks(criterion: &mut Criterion) {
    let config = Config::defaults("Todo".to_owned());
    let body = format!(
        "{}\n",
        "task body with [[links]] and `code`\n".repeat(2_048)
    );
    let source = format!(
        "---\nname: \"Large task\"\nstate: open\nprojects: [\"[[Todo/Projects/work]]\"]\ntags: [review, performance]\ndue_date: 2028-02-29\nrecurrence: \"FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29\"\nrecurrence_from: schedule\nplugin:\n  color: blue\n  values: [1, two, null]\n---\n{body}"
    );
    let id = "01K4B0ZSBZZV25T1K0D3TA8JHR";
    let path = Path::new("Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md");
    let mut group = criterion.benchmark_group("frontmatter");
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("parse_large_record", |bencher| {
        bencher.iter(|| {
            parse_task(
                black_box(id),
                black_box(path),
                black_box(source.as_bytes()),
                black_box(&config),
            )
            .expect("parse task")
        });
    });
    let task = parse_task(id, path, source.as_bytes(), &config).expect("parse fixture");
    group.bench_function("serialize_large_record", |bencher| {
        bencher
            .iter(|| serialize_task(black_box(&task), black_box(&config)).expect("serialize task"));
    });
    group.finish();
}

fn scan_benchmarks(criterion: &mut Criterion) {
    let (temporary, store) = populated_store(1_000);
    let mut group = criterion.benchmark_group("store_scan");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(5));
    group.throughput(Throughput::Elements(1_000));
    group.bench_with_input(
        BenchmarkId::new("valid_tasks", 1_000),
        &store,
        |bencher, store| {
            bencher.iter(|| black_box(store.list_tasks().expect("scan tasks")));
        },
    );
    group.finish();
    drop(temporary);
}

fn populated_store(count: usize) -> (TempDir, Store) {
    let temporary = TempDir::new().expect("benchmark directory");
    let root = temporary.path().join("Todo");
    initialize(&InitOptions {
        store_path: &root,
        vault_root: Some(temporary.path()),
        current_directory: temporary.path(),
        adopt_empty_layout: false,
        dry_run: false,
    })
    .expect("initialize benchmark store");
    let config = Config::defaults("Todo".to_owned());
    for index in 0..count {
        let id = Ulid::new().to_string();
        let path = Path::new("Tasks").join(format!("{id}.md"));
        let task = Task {
            id: id.clone(),
            path: path.clone(),
            name: format!("Benchmark task {index:04}"),
            state: "open".to_owned(),
            projects: Vec::new(),
            tags: vec!["benchmark".to_owned()],
            due_date: Some(date("2026-09-02")),
            recurrence: None,
            recurrence_from: None,
            last_completed_date: None,
            body: "Benchmark body.\n".to_owned(),
            extra_properties: Mapping::new(),
        };
        fs::write(
            root.join(path),
            serialize_task(&task, &config).expect("serialize fixture"),
        )
        .expect("write fixture");
    }
    let store = Store::open(&root).expect("open benchmark store");
    (temporary, store)
}

criterion_group!(
    benches,
    recurrence_benchmarks,
    frontmatter_benchmarks,
    scan_benchmarks
);
criterion_main!(benches);
