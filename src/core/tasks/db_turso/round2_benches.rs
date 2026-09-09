use super::*;

const PART_ROWS: usize = 433_248;
const FILE_ROWS: usize = 864;
const SECONDARY_INDEX: &str = "idx_subfiles_file_id_data_order";
const PART_COLUMNS: &str = "id, file_id, path, remote_length, local_length, remote_start, local_start, remote_checksum, local_checksum, data_order";

async fn isolated_database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = build_and_bootstrap(dir.path().join("database.db").to_str().unwrap())
        .await
        .unwrap();
    (dir, db)
}

async fn explain(conn: &TunedConnection, label: &str, sql: &str, binds: Vec<turso::Value>) {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), binds)
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        println!("[round2 plan] {label}: {:?}", row.get_value(3).unwrap());
    }
}

async fn seed_parts(conn: &TunedConnection) {
    conn.execute("BEGIN", ()).await.unwrap();
    for file in 1..=FILE_ROWS {
        conn.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, 'remote', 'local')",
            (file as i64, format!("f{file}")),
        ).await.unwrap();
    }
    for start in (0..PART_ROWS).step_by(256) {
        let end = (start + 256).min(PART_ROWS);
        let placeholders = vec!["(?, ?, ?, ?, 64, 0, 64, 0, '', 'remote')"; end - start].join(",");
        let sql = format!(
            "INSERT INTO subfiles (id, file_id, path, data_order, remote_length, remote_start, local_length, local_start, local_checksum, remote_checksum) VALUES {placeholders}"
        );
        let mut values = Vec::with_capacity((end - start) * 4);
        for index in start..end {
            let file = index * FILE_ROWS / PART_ROWS + 1;
            let order = index - ((file - 1) * PART_ROWS).div_ceil(FILE_ROWS);
            values.extend([
                turso::Value::Integer(index as i64 + 1),
                turso::Value::Integer(file as i64),
                turso::Value::Text(format!("p{order:05}")),
                turso::Value::Integer(order as i64),
            ]);
        }
        conn.execute(&sql, values).await.unwrap();
    }
    conn.execute("COMMIT", ()).await.unwrap();
}

async fn read_fingerprint(conn: &TunedConnection, sql: &str, columns: usize) -> (usize, [u8; 32]) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut count = 0;
    let mut fingerprint = [0u8; 32];
    while let Some(row) = rows.next().await.unwrap() {
        let mut hasher = blake3::Hasher::new();
        for column in 0..columns {
            hasher.update(format!("{:?}|", row.get_value(column).unwrap()).as_bytes());
        }
        for (sum, byte) in fingerprint.iter_mut().zip(hasher.finalize().as_bytes()) {
            *sum = sum.wrapping_add(*byte);
        }
        count += 1;
    }
    (count, fingerprint)
}

async fn drain_read(conn: &TunedConnection, sql: &str, columns: usize) -> usize {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut count = 0;
    while let Some(row) = rows.next().await.unwrap() {
        for column in 0..columns {
            std::hint::black_box(row.get_value(column).unwrap());
        }
        count += 1;
    }
    count
}

#[tokio::test]
#[ignore = "release performance benchmark; run manually with --ignored --nocapture"]
async fn bench_round2_progress_join() {
    let (_dir, db) = isolated_database().await;
    let conn = connect_tuned(&db).await.unwrap();
    for file in 1..=FILE_ROWS + 2 {
        conn.execute("INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size, download_total, download_cycle) VALUES (?, 'remote', 'local', 100000, 7, 9)", [file as i64]).await.unwrap();
    }
    let chunk = crate::core::tasks::init_database::bulk_write_rows_for(3);
    for join in [false, true] {
        let label = if join { "join" } else { "correlated" };
        let mut times = Vec::new();
        for iteration in 0..4 {
            conn.execute(
                "UPDATE download_target_file SET download_total = 7, download_cycle = 9",
                (),
            )
            .await
            .unwrap();
            let mut statements = Vec::new();
            for start in (1..=FILE_ROWS).step_by(chunk) {
                let end = (start + chunk).min(FILE_ROWS + 1);
                let placeholders = vec!["(?, ?, ?)"; end - start].join(",");
                let assignment = if join {
                    "SET download_total = progress.download_total, download_cycle = progress.download_cycle FROM progress WHERE download_target_file.file_id = progress.file_id"
                } else {
                    "SET download_total = (SELECT download_total FROM progress WHERE progress.file_id = download_target_file.file_id), download_cycle = (SELECT download_cycle FROM progress WHERE progress.file_id = download_target_file.file_id) WHERE file_id IN (SELECT file_id FROM progress)"
                };
                let sql = format!(
                    "WITH progress(file_id, download_total, download_cycle) AS (VALUES {placeholders}) UPDATE download_target_file {assignment}"
                );
                let values: Vec<turso::Value> = (start..end)
                    .flat_map(|file| [file as i64, file as i64 * 31, file as i64 * 3])
                    .map(turso::Value::Integer)
                    .collect();
                if iteration == 0 && start == 1 {
                    explain(&conn, label, &sql, values.clone()).await;
                }
                statements.push((sql, values));
            }
            let started = Instant::now();
            conn.execute("BEGIN", ()).await.unwrap();
            let mut affected = 0;
            for (sql, values) in statements {
                affected += conn.execute(&sql, values).await.unwrap();
            }
            conn.execute("COMMIT", ()).await.unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            assert_eq!(affected, FILE_ROWS as u64);
            let mut rows = conn.query("SELECT file_id, download_total, download_cycle FROM download_target_file ORDER BY file_id", ()).await.unwrap();
            let mut count = 0;
            while let Some(row) = rows.next().await.unwrap() {
                let file = row.get::<i64>(0).unwrap();
                assert_eq!(
                    row.get::<i64>(1).unwrap(),
                    if file <= FILE_ROWS as i64 {
                        file * 31
                    } else {
                        7
                    }
                );
                assert_eq!(
                    row.get::<i64>(2).unwrap(),
                    if file <= FILE_ROWS as i64 {
                        file * 3
                    } else {
                        9
                    }
                );
                count += 1;
            }
            assert_eq!(count, FILE_ROWS + 2);
            if iteration > 0 {
                times.push(elapsed);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "[round2 progress] arm={label} rows={FILE_ROWS} chunk={chunk} median_s={:.6} samples={times:?}",
            times[1]
        );
    }
}

#[tokio::test]
#[ignore = "release performance benchmark; run manually with --ignored --nocapture"]
async fn bench_round2_subfiles_read_indexes() {
    let (_dir, db) = isolated_database().await;
    let conn = connect_tuned(&db).await.unwrap();
    seed_parts(&conn).await;
    let ids = (1..=FILE_ROWS)
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let shapes = [
        (
            "tree_ordered",
            format!(
                "SELECT {PART_COLUMNS} FROM subfiles WHERE file_id IN ({ids}) ORDER BY file_id, data_order, id"
            ),
            10,
        ),
        (
            "tree_unordered",
            format!("SELECT {PART_COLUMNS} FROM subfiles WHERE file_id IN ({ids})"),
            10,
        ),
        (
            "pending_ordered",
            format!(
                "SELECT {PART_COLUMNS} FROM subfiles WHERE file_id IN ({ids}) ORDER BY data_order"
            ),
            10,
        ),
        (
            "per_file_ordered",
            format!(
                "SELECT {PART_COLUMNS} FROM subfiles WHERE file_id = 432 ORDER BY data_order, id"
            ),
            10,
        ),
        (
            "quick_scan_group",
            format!(
                "SELECT file_id, COUNT(*), SUM(remote_length) FROM subfiles WHERE file_id IN ({ids}) GROUP BY file_id"
            ),
            3,
        ),
        (
            "remote_files_join",
            format!(
                "SELECT f.id, COUNT(sf.id), SUM(CASE WHEN sf.id IS NOT NULL AND sf.remote_checksum = '' THEN 1 ELSE 0 END) FROM files f LEFT JOIN subfiles sf ON sf.file_id = f.id WHERE f.id IN ({ids}) GROUP BY f.id"
            ),
            3,
        ),
        (
            "part_path_lookup",
            "SELECT id, remote_checksum FROM subfiles WHERE file_id = 432 AND path = 'p00001'"
                .to_owned(),
            2,
        ),
    ];
    let mut expected = Vec::new();
    for secondary in [true, false] {
        let conn = connect_tuned(&db).await.unwrap();
        if secondary {
            conn.execute(&format!("CREATE INDEX IF NOT EXISTS {SECONDARY_INDEX} ON subfiles(file_id, data_order, id)"), ()).await.unwrap();
        } else {
            conn.execute(&format!("DROP INDEX {SECONDARY_INDEX}"), ())
                .await
                .unwrap();
        }
        for (index, (label, sql, columns)) in shapes.iter().enumerate() {
            explain(&conn, label, sql, vec![]).await;
            let result = read_fingerprint(&conn, sql, *columns).await;
            if secondary {
                expected.push(result);
            } else {
                assert_eq!(result, expected[index], "{label}");
            }
            let mut times = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                assert_eq!(drain_read(&conn, sql, *columns).await, result.0);
                times.push(started.elapsed().as_secs_f64());
            }
            times.sort_by(f64::total_cmp);
            println!(
                "[round2 read] secondary={secondary} shape={label} rows={} median_s={:.6} samples={times:?}",
                result.0, times[1]
            );
        }
    }
}

#[tokio::test]
#[ignore = "release performance benchmark; run manually with --ignored --nocapture"]
async fn bench_round2_purge_index_cost() {
    for drop_indexes in [false, true] {
        let (_dir, db) = isolated_database().await;
        let conn = connect_tuned(&db).await.unwrap();
        seed_parts(&conn).await;
        conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
        let started = Instant::now();
        conn.execute("BEGIN", ()).await.unwrap();
        if drop_indexes {
            for name in SUBFILES_INDEX_NAMES {
                conn.execute(&format!("DROP INDEX {name}"), ())
                    .await
                    .unwrap();
            }
        }
        let drop_s = started.elapsed().as_secs_f64();
        let delete_started = Instant::now();
        let affected = conn
            .execute(
                "DELETE FROM subfiles WHERE file_id IN (SELECT id FROM files)",
                (),
            )
            .await
            .unwrap();
        let delete_s = delete_started.elapsed().as_secs_f64();
        if drop_indexes {
            for sql in SUBFILES_INDEX_CREATE_SQL {
                conn.execute(sql, ()).await.unwrap();
            }
        }
        conn.execute("COMMIT", ()).await.unwrap();
        assert_eq!(affected, PART_ROWS as u64);
        assert_eq!(
            read_fingerprint(&conn, "SELECT id FROM subfiles", 1)
                .await
                .0,
            0
        );
        println!(
            "[round2 purge] drop_indexes={drop_indexes} rows={affected} drop_s={drop_s:.6} delete_s={delete_s:.6} total_s={:.6}",
            started.elapsed().as_secs_f64()
        );
    }
}
