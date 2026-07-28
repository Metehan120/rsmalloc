use std::{
    collections::HashMap,
    hint::black_box,
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread::{self, JoinHandle},
    time::Duration,
};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rusqlite::{Connection, Result, params};

const ACCOUNT_COUNT: usize = 2_000;
const INITIAL_EVENT_COUNT: usize = 12_000;
const POINT_READS_PER_ROUND: usize = 128;
const INSERTS_PER_TRANSACTION: usize = 256;
const PIPELINE_MESSAGES_PER_ROUND: usize = 4_096;
const PIPELINE_QUEUE_CAPACITY: usize = 128;

fn payload_for(id: usize) -> String {
    let repeated = 24 + id % 160;
    format!(
        "{{\"event_id\":{id},\"source\":\"worker-{}\",\"message\":\"{}\"}}",
        id % 32,
        "application-data-".repeat(repeated / 17 + 1)
    )
}

fn populated_database() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = MEMORY;

         CREATE TABLE accounts (
             id       INTEGER PRIMARY KEY,
             tenant   INTEGER NOT NULL,
             name     TEXT NOT NULL,
             plan     TEXT NOT NULL
         );

         CREATE TABLE events (
             id          INTEGER PRIMARY KEY,
             account_id  INTEGER NOT NULL,
             category    INTEGER NOT NULL,
             score       INTEGER NOT NULL,
             active      INTEGER NOT NULL,
             payload     TEXT NOT NULL,
             created_at  INTEGER NOT NULL,
             FOREIGN KEY(account_id) REFERENCES accounts(id)
         );

         CREATE INDEX events_account_created
             ON events(account_id, created_at DESC);
         CREATE INDEX events_category_active
             ON events(category, active);",
    )?;

    let transaction = connection.transaction()?;
    {
        let mut insert_account = transaction
            .prepare("INSERT INTO accounts(id, tenant, name, plan) VALUES (?1, ?2, ?3, ?4)")?;
        for id in 1..=ACCOUNT_COUNT {
            let plan = match id % 10 {
                0 => "enterprise",
                1..=3 => "pro",
                _ => "free",
            };
            insert_account.execute(params![
                id as i64,
                (id % 64) as i64,
                format!("account-{id}"),
                plan
            ])?;
        }

        let mut insert_event = transaction.prepare(
            "INSERT INTO events(
                 id, account_id, category, score, active, payload, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for id in 1..=INITIAL_EVENT_COUNT {
            insert_event.execute(params![
                id as i64,
                (id % ACCOUNT_COUNT + 1) as i64,
                (id % 24) as i64,
                ((id * 37) % 10_000) as i64,
                (id % 7 != 0) as i64,
                payload_for(id),
                1_700_000_000_i64 + id as i64,
            ])?;
        }
    }
    transaction.commit()?;

    Ok(connection)
}

fn run_read_round(connection: &Connection, round: &mut usize) -> Result<u64> {
    let mut checksum = 0_u64;
    let mut point_read = connection.prepare(
        "SELECT e.score, e.payload, a.tenant, a.plan
         FROM events e
         JOIN accounts a ON a.id = e.account_id
         WHERE e.account_id = ?1
         ORDER BY e.created_at DESC
         LIMIT 8",
    )?;

    for request in 0..POINT_READS_PER_ROUND {
        let account_id = ((*round * 131 + request * 17) % ACCOUNT_COUNT + 1) as i64;
        let rows = point_read.query_map([account_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;

        for row in rows {
            let (score, payload, tenant, plan) = row?;
            checksum = checksum
                .wrapping_add(score as u64)
                .wrapping_add(payload.len() as u64)
                .wrapping_add(tenant as u64)
                .wrapping_add(plan.len() as u64);
        }
    }

    let mut report = connection.prepare(
        "SELECT a.tenant, e.category, COUNT(*), AVG(e.score), SUM(LENGTH(e.payload))
         FROM events e
         JOIN accounts a ON a.id = e.account_id
         WHERE e.active = 1 AND e.score >= ?1
         GROUP BY a.tenant, e.category
         ORDER BY COUNT(*) DESC, a.tenant
         LIMIT 64",
    )?;
    let rows = report.query_map([5_000_i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (tenant, category, count, average, bytes) = row?;
        checksum = checksum
            .wrapping_add(tenant as u64)
            .wrapping_add(category as u64)
            .wrapping_add(count as u64)
            .wrapping_add(average as u64)
            .wrapping_add(bytes as u64);
    }

    *round = round.wrapping_add(1);
    Ok(checksum)
}

fn run_transaction_round(connection: &mut Connection) -> Result<u64> {
    let transaction = connection.transaction()?;
    let mut checksum = 0_u64;

    {
        let mut insert = transaction.prepare(
            "INSERT INTO events(
                 id, account_id, category, score, active, payload, created_at
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
        )?;
        for offset in 0..INSERTS_PER_TRANSACTION {
            let id = INITIAL_EVENT_COUNT + offset + 1;
            insert.execute(params![
                id as i64,
                (id % ACCOUNT_COUNT + 1) as i64,
                (id % 24) as i64,
                ((id * 41) % 10_000) as i64,
                payload_for(id),
                1_800_000_000_i64 + id as i64,
            ])?;
        }
    }

    transaction.execute(
        "UPDATE events
         SET score = score + 25,
             active = CASE WHEN score + 25 > 9000 THEN 0 ELSE active END
         WHERE category IN (3, 7, 11, 19)",
        [],
    )?;

    {
        let mut statement = transaction.prepare(
            "SELECT category, COUNT(*), SUM(score), SUM(LENGTH(payload))
             FROM events
             WHERE active = 1
             GROUP BY category
             ORDER BY SUM(score) DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (category, count, score, bytes) = row?;
            checksum = checksum
                .wrapping_add(category as u64)
                .wrapping_add(count as u64)
                .wrapping_add(score as u64)
                .wrapping_add(bytes as u64);
        }
    }

    transaction.execute(
        "DELETE FROM events WHERE id > ?1 AND id % 5 = 0",
        [INITIAL_EVENT_COUNT as i64],
    )?;
    transaction.commit()?;

    Ok(checksum)
}

enum ProducerCommand {
    Run {
        batch_id: usize,
        start: usize,
        count: usize,
        expected: usize,
    },
    Stop,
}

enum ProcessorCommand {
    Message(RawMessage),
    Stop,
}

enum AggregatorCommand {
    Message(ProcessedMessage),
    Stop,
}

struct RawMessage {
    batch_id: usize,
    expected: usize,
    id: usize,
    tenant: String,
    route: String,
    headers: Vec<(String, String)>,
    payload: Vec<u8>,
}

struct ProcessedMessage {
    batch_id: usize,
    expected: usize,
    tenant: usize,
    status: u16,
    response: String,
    tags: Vec<String>,
    checksum: u64,
}

struct BatchResult {
    batch_id: usize,
    checksum: u64,
}

struct Pipeline {
    producer_commands: Vec<Sender<ProducerCommand>>,
    processor_commands: Vec<SyncSender<ProcessorCommand>>,
    aggregator_commands: SyncSender<AggregatorCommand>,
    completed: Receiver<BatchResult>,
    producer_handles: Vec<JoinHandle<()>>,
    processor_handles: Vec<JoinHandle<()>>,
    aggregator_handle: Option<JoinHandle<()>>,
    next_batch: usize,
}

fn message_payload_size(id: usize) -> usize {
    match id % 100 {
        0..=79 => 192 + id % 320,
        80..=94 => 2 * 1024 + id % 2_048,
        95..=98 => 12 * 1024 + id % (8 * 1024),
        _ => 96 * 1024 + id % (32 * 1024),
    }
}

fn build_raw_message(batch_id: usize, expected: usize, id: usize) -> RawMessage {
    let payload_size = message_payload_size(id);
    let prefix = format!(
        "request_id={id};tenant={};route=/v1/items/{};body=",
        id % 128,
        id % 2_048
    );
    let mut payload = Vec::with_capacity(payload_size);
    payload.extend_from_slice(prefix.as_bytes());
    payload.resize(payload_size, b'a' + (id % 26) as u8);

    RawMessage {
        batch_id,
        expected,
        id,
        tenant: format!("tenant-{}", id % 128),
        route: format!("/v1/items/{}", id % 2_048),
        headers: vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("x-request-id".to_owned(), format!("req-{batch_id}-{id}")),
            (
                "user-agent".to_owned(),
                format!("pipeline-client/{}", id % 8),
            ),
        ],
        payload,
    }
}

fn process_message(message: RawMessage) -> ProcessedMessage {
    let mut checksum = message.id as u64;
    for chunk in message.payload.chunks(64) {
        checksum = chunk.iter().fold(checksum.rotate_left(5), |value, byte| {
            value.wrapping_mul(16_777_619) ^ u64::from(*byte)
        });
    }
    for (name, value) in &message.headers {
        checksum = checksum
            .wrapping_add(name.len() as u64)
            .wrapping_add(value.len() as u64);
    }

    let tenant = message.id % 128;
    let status = if message.id.is_multiple_of(37) {
        503
    } else {
        200
    };
    let tags = vec![
        message.tenant,
        format!("route:items-{}", message.id % 16),
        if status == 200 { "ok" } else { "retry" }.to_owned(),
    ];
    let response = format!(
        "{{\"request_id\":{},\"route\":\"{}\",\"status\":{status},\"bytes\":{},\"checksum\":{checksum}}}",
        message.id,
        message.route,
        message.payload.len()
    );

    ProcessedMessage {
        batch_id: message.batch_id,
        expected: message.expected,
        tenant,
        status,
        response,
        tags,
        checksum,
    }
}

fn run_aggregator(commands: Receiver<AggregatorCommand>, completed: Sender<BatchResult>) {
    let mut current_batch = None;
    let mut expected = 0_usize;
    let mut received = 0_usize;
    let mut checksum = 0_u64;
    let mut tenant_counts = HashMap::<usize, usize>::new();

    while let Ok(command) = commands.recv() {
        let AggregatorCommand::Message(message) = command else {
            break;
        };

        match current_batch {
            None => {
                current_batch = Some(message.batch_id);
                expected = message.expected;
            }
            Some(batch_id) => assert_eq!(batch_id, message.batch_id),
        }

        *tenant_counts.entry(message.tenant).or_default() += 1;
        checksum = checksum
            .wrapping_add(message.checksum)
            .wrapping_add(u64::from(message.status))
            .wrapping_add(message.response.len() as u64)
            .wrapping_add(message.tags.iter().map(String::len).sum::<usize>() as u64);
        received += 1;

        if received == expected {
            checksum = tenant_counts
                .values()
                .fold(checksum, |value, count| value.wrapping_add(*count as u64));

            completed
                .send(BatchResult {
                    batch_id: current_batch.expect("pipeline batch id missing"),
                    checksum,
                })
                .expect("pipeline completion receiver disconnected");

            current_batch = None;
            expected = 0;
            received = 0;
            checksum = 0;
            tenant_counts.clear();
        }
    }
}

impl Pipeline {
    fn new(producer_count: usize, processor_count: usize) -> Self {
        let (aggregator_commands, aggregator_receiver) =
            mpsc::sync_channel(PIPELINE_QUEUE_CAPACITY);
        let (completed_sender, completed) = mpsc::channel();
        let aggregator_handle = thread::spawn(move || {
            run_aggregator(aggregator_receiver, completed_sender);
        });

        let mut processor_commands = Vec::with_capacity(processor_count);
        let mut processor_handles = Vec::with_capacity(processor_count);
        for _ in 0..processor_count {
            let (commands, receiver) = mpsc::sync_channel(PIPELINE_QUEUE_CAPACITY);
            let aggregator = aggregator_commands.clone();
            processor_commands.push(commands);
            processor_handles.push(thread::spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        ProcessorCommand::Message(message) => aggregator
                            .send(AggregatorCommand::Message(process_message(message)))
                            .expect("pipeline aggregator disconnected"),
                        ProcessorCommand::Stop => break,
                    }
                }
            }));
        }

        let mut producer_commands = Vec::with_capacity(producer_count);
        let mut producer_handles = Vec::with_capacity(producer_count);
        for producer_id in 0..producer_count {
            let (commands, receiver) = mpsc::channel();
            let processors = processor_commands.clone();
            producer_commands.push(commands);
            producer_handles.push(thread::spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        ProducerCommand::Run {
                            batch_id,
                            start,
                            count,
                            expected,
                        } => {
                            for id in start..start + count {
                                let target = (id + producer_id) % processors.len();
                                processors[target]
                                    .send(ProcessorCommand::Message(build_raw_message(
                                        batch_id, expected, id,
                                    )))
                                    .expect("pipeline processor disconnected");
                            }
                        }
                        ProducerCommand::Stop => break,
                    }
                }
            }));
        }

        Self {
            producer_commands,
            processor_commands,
            aggregator_commands,
            completed,
            producer_handles,
            processor_handles,
            aggregator_handle: Some(aggregator_handle),
            next_batch: 0,
        }
    }

    fn run_round(&mut self, message_count: usize) -> u64 {
        let batch_id = self.next_batch;
        self.next_batch = self.next_batch.wrapping_add(1);

        let base = message_count / self.producer_commands.len();
        let remainder = message_count % self.producer_commands.len();
        let mut start = batch_id.wrapping_mul(message_count);
        for (producer_id, commands) in self.producer_commands.iter().enumerate() {
            let count = base + usize::from(producer_id < remainder);
            commands
                .send(ProducerCommand::Run {
                    batch_id,
                    start,
                    count,
                    expected: message_count,
                })
                .expect("pipeline producer disconnected");
            start = start.wrapping_add(count);
        }

        let result = self
            .completed
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|error| {
                panic!("pipeline batch {batch_id} did not complete within 30 seconds: {error}")
            });
        assert_eq!(result.batch_id, batch_id);
        result.checksum
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        for commands in &self.producer_commands {
            let _ = commands.send(ProducerCommand::Stop);
        }
        for handle in self.producer_handles.drain(..) {
            handle.join().expect("pipeline producer panicked");
        }

        for commands in &self.processor_commands {
            let _ = commands.send(ProcessorCommand::Stop);
        }
        for handle in self.processor_handles.drain(..) {
            handle.join().expect("pipeline processor panicked");
        }

        let _ = self.aggregator_commands.send(AggregatorCommand::Stop);
        if let Some(handle) = self.aggregator_handle.take() {
            handle.join().expect("pipeline aggregator panicked");
        }
    }
}

fn bench_sqlite_application(c: &mut Criterion) {
    let connection = populated_database().expect("failed to build SQLite read database");
    let mut round = 0_usize;

    let mut reads = c.benchmark_group("rstress_app_sqlite_in_memory_reads");
    reads.warm_up_time(Duration::from_secs(2));
    reads.measurement_time(Duration::from_secs(10));
    reads.throughput(Throughput::Elements(1));
    reads.bench_function("indexed_reads_join_and_aggregate", |b| {
        b.iter(|| {
            let checksum =
                run_read_round(&connection, &mut round).expect("SQLite read workload failed");
            black_box(checksum)
        });
    });
    reads.finish();
    drop(connection);

    let mut transactions = c.benchmark_group("rstress_app_sqlite_in_memory_transactions");
    transactions.warm_up_time(Duration::from_secs(1));
    transactions.measurement_time(Duration::from_secs(10));
    transactions.sample_size(20);
    transactions.throughput(Throughput::Elements(1));
    transactions.bench_function("write_update_report_and_delete", |b| {
        b.iter_batched_ref(
            || populated_database().expect("failed to build SQLite transaction database"),
            |connection| {
                let checksum =
                    run_transaction_round(connection).expect("SQLite transaction workload failed");
                black_box(checksum)
            },
            BatchSize::LargeInput,
        );
    });
    transactions.finish();
}

fn bench_producer_consumer_pipeline(c: &mut Criterion) {
    let cpus = thread::available_parallelism().map_or(1, |count| count.get());
    let worker_budget = cpus.saturating_sub(2).max(2);
    let producer_count = (worker_budget / 3).clamp(1, 4);
    let processor_count = worker_budget.saturating_sub(producer_count).clamp(1, 12);
    let mut pipeline = Pipeline::new(producer_count, processor_count);

    let mut group = c.benchmark_group("rstress_app_producer_consumer");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(PIPELINE_MESSAGES_PER_ROUND as u64));
    group.bench_function(
        format!("bounded_pipeline_{producer_count}p_{processor_count}c"),
        |b| {
            b.iter(|| {
                black_box(pipeline.run_round(PIPELINE_MESSAGES_PER_ROUND));
            });
        },
    );
    group.finish();
}

criterion_group!(
    benches,
    bench_sqlite_application,
    bench_producer_consumer_pipeline
);
criterion_main!(benches);
