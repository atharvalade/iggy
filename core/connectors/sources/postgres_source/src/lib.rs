/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 *
 *   http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

use async_trait::async_trait;
use humantime::Duration as HumanDuration;
use iggy_common::{DateTime, Utc};
use iggy_connector_sdk::{
    ConnectorState, Error, ProducedMessage, ProducedMessages, Schema, Source, source_connector,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Column, Pool, Postgres, Row, TypeInfo};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

source_connector!(PostgresSource);

const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_DELAY: &str = "1s";

#[derive(Debug)]
pub struct PostgresSource {
    pub id: u32,
    pool: Option<Pool<Postgres>>,
    config: PostgresSourceConfig,
    state: Mutex<State>,
    verbose: bool,
    retry_delay: Duration,
    poll_interval: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresSourceConfig {
    #[serde(serialize_with = "iggy_common::serde_secret::serialize_secret")]
    pub connection_string: SecretString,
    pub mode: String,
    pub tables: Vec<String>,
    pub poll_interval: Option<String>,
    pub batch_size: Option<u32>,
    pub tracking_column: Option<String>,
    pub initial_offset: Option<String>,
    pub max_connections: Option<u32>,
    pub enable_wal_cdc: Option<bool>,
    pub custom_query: Option<String>,
    pub snake_case_columns: Option<bool>,
    pub include_metadata: Option<bool>,
    pub replication_slot: Option<String>,
    pub publication_name: Option<String>,
    pub capture_operations: Option<Vec<String>>,
    pub cdc_backend: Option<String>,
    pub delete_after_read: Option<bool>,
    pub processed_column: Option<String>,
    pub primary_key_column: Option<String>,
    pub payload_column: Option<String>,
    pub payload_format: Option<String>,
    pub verbose_logging: Option<bool>,
    pub max_retries: Option<u32>,
    pub retry_delay: Option<String>,
    /// Namespace prefix for table names in emitted records (e.g. "tpch" -> "tpch.lineitem").
    /// Required for Iceberg dynamic routing which expects "namespace.table" format.
    pub table_namespace: Option<String>,
    /// Process tables concurrently instead of sequentially during polling.
    pub parallel_tables: Option<bool>,
    /// Number of row-range chunks to split large tables into for parallel reads.
    /// Only used when parallel_tables is enabled and mode is "snapshot" or "polling".
    pub chunk_size: Option<u64>,
    /// Snapshot mode: "full" performs an initial bulk read of all existing data
    /// before switching to CDC. Requires mode = "cdc".
    pub snapshot_mode: Option<String>,
    /// When true, emit flat JSON with column values at the top level alongside
    /// `table_name`, instead of wrapping in a DatabaseRecord envelope.
    /// Useful for sinks (like Iceberg) that need JSON matching the target schema.
    pub flat_json_output: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PayloadFormat {
    #[default]
    Json,
    Bytea,
    Text,
    JsonDirect,
}

impl PayloadFormat {
    fn from_config(s: Option<&str>) -> Self {
        match s.map(|s| s.to_lowercase()).as_deref() {
            Some("bytea") | Some("raw") => PayloadFormat::Bytea,
            Some("text") => PayloadFormat::Text,
            Some("json_direct") | Some("jsonb") | Some("jsonb_direct") => PayloadFormat::JsonDirect,
            _ => PayloadFormat::Json,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    last_poll_time: DateTime<Utc>,
    tracking_offsets: HashMap<String, String>,
    processed_rows: u64,
    snapshot_completed: bool,
    snapshot_tables_done: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DatabaseRecord {
    pub table_name: String,
    pub operation_type: String,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
    pub old_data: Option<serde_json::Value>,
}

const CONNECTOR_NAME: &str = "PostgreSQL source";

impl PostgresSource {
    pub fn new(id: u32, config: PostgresSourceConfig, state: Option<ConnectorState>) -> Self {
        let verbose = config.verbose_logging.unwrap_or(false);
        let restored_state = state
            .and_then(|s| s.deserialize::<State>(CONNECTOR_NAME, id))
            .inspect(|s| {
                info!(
                    "Restored state for {CONNECTOR_NAME} connector with ID: {id}. \
                     Tracking offsets: {:?}, processed rows: {}",
                    s.tracking_offsets, s.processed_rows
                );
            });

        let delay_str = config.retry_delay.as_deref().unwrap_or(DEFAULT_RETRY_DELAY);
        let retry_delay = HumanDuration::from_str(delay_str)
            .map(|duration| duration.into())
            .unwrap_or_else(|_| Duration::from_secs(1));
        let interval_str = config.poll_interval.as_deref().unwrap_or("10s");
        let poll_interval = HumanDuration::from_str(interval_str)
            .map(|duration| duration.into())
            .unwrap_or_else(|_| Duration::from_secs(10));
        PostgresSource {
            id,
            pool: None,
            config,
            state: Mutex::new(restored_state.unwrap_or(State {
                last_poll_time: Utc::now(),
                tracking_offsets: HashMap::new(),
                processed_rows: 0,
                snapshot_completed: false,
                snapshot_tables_done: Vec::new(),
            })),
            verbose,
            retry_delay,
            poll_interval,
        }
    }

    fn serialize_state(&self, state: &State) -> Option<ConnectorState> {
        ConnectorState::serialize(state, CONNECTOR_NAME, self.id)
    }
}

#[async_trait]
impl Source for PostgresSource {
    async fn open(&mut self) -> Result<(), Error> {
        info!(
            "Opening PostgreSQL source connector with ID: {}. Mode: {}, Tables: {:?}",
            self.id, self.config.mode, self.config.tables
        );

        self.connect().await?;

        match self.config.mode.as_str() {
            "cdc" => {
                let snapshot = self.config.snapshot_mode.as_deref().unwrap_or("none");
                if snapshot == "full" {
                    let is_done = self.state.lock().await.snapshot_completed;
                    if !is_done {
                        info!(
                            "Snapshot mode 'full' enabled for connector ID: {}. Will bulk-read all tables before switching to CDC.",
                            self.id
                        );
                    }
                    self.setup_cdc().await?;
                } else {
                    self.setup_cdc().await?;
                }
                let backend = self.config.cdc_backend.as_deref().unwrap_or("builtin");
                info!(
                    "PostgreSQL CDC mode enabled (backend: {backend}, snapshot: {snapshot}) for connector ID: {}",
                    self.id
                );
            }
            "polling" => {
                info!(
                    "PostgreSQL polling mode enabled for connector ID: {}",
                    self.id
                );
                info!("Poll interval: {:?}", self.poll_interval);
            }
            _ => {
                return Err(Error::InitError(format!(
                    "Invalid mode '{}'. Supported modes: 'polling', 'cdc'",
                    self.config.mode
                )));
            }
        }

        info!(
            "PostgreSQL source connector with ID: {} opened successfully",
            self.id
        );
        Ok(())
    }

    async fn poll(&self) -> Result<ProducedMessages, Error> {
        let poll_interval = self.poll_interval;
        tokio::time::sleep(poll_interval).await;

        let messages = match self.config.mode.as_str() {
            "polling" => match self.poll_tables().await {
                Ok(msgs) => msgs,
                Err(e) => {
                    error!("poll_tables error: {:?}", e);
                    return Err(e);
                }
            },
            "cdc" => {
                let snapshot_mode = self.config.snapshot_mode.as_deref().unwrap_or("none");
                if snapshot_mode == "full" {
                    let is_done = self.state.lock().await.snapshot_completed;
                    if !is_done {
                        return self.poll_snapshot_phase().await;
                    }
                }
                self.poll_cdc().await?
            }
            _ => {
                error!("Invalid mode: {}", self.config.mode);
                return Err(Error::InvalidConfig);
            }
        };

        let state = self.state.lock().await;
        if self.verbose {
            info!(
                "PostgreSQL source connector ID: {} produced {} messages. Total processed: {}",
                self.id,
                messages.len(),
                state.processed_rows
            );
        } else {
            debug!(
                "PostgreSQL source connector ID: {} produced {} messages. Total processed: {}",
                self.id,
                messages.len(),
                state.processed_rows
            );
        }

        let schema = match self.payload_format() {
            PayloadFormat::Bytea => Schema::Raw,
            PayloadFormat::Text => Schema::Text,
            PayloadFormat::JsonDirect | PayloadFormat::Json => Schema::Json,
        };

        let persisted_state = self.serialize_state(&state);

        Ok(ProducedMessages {
            schema,
            messages,
            state: persisted_state,
        })
    }

    async fn close(&mut self) -> Result<(), Error> {
        if let Some(pool) = self.pool.take() {
            pool.close().await;
            info!(
                "PostgreSQL connection pool closed for connector ID: {}",
                self.id
            );
        }

        let state = self.state.lock().await;
        info!(
            "PostgreSQL source connector ID: {} closed. Total rows processed: {}",
            self.id, state.processed_rows
        );
        Ok(())
    }
}

impl PostgresSource {
    async fn connect(&mut self) -> Result<(), Error> {
        let max_connections = self.config.max_connections.unwrap_or(10);
        let redacted = redact_connection_string(self.config.connection_string.expose_secret());

        info!("Connecting to PostgreSQL with max {max_connections} connections: {redacted}");

        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(self.config.connection_string.expose_secret())
            .await
            .map_err(|e| Error::InitError(format!("Failed to connect to PostgreSQL: {e}")))?;

        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| Error::InitError(format!("Database connectivity test failed: {e}")))?;

        self.pool = Some(pool);
        info!("Connected to PostgreSQL database with {max_connections} max connections");
        Ok(())
    }

    async fn setup_cdc(&self) -> Result<(), Error> {
        if !self.config.enable_wal_cdc.unwrap_or(false) {
            return Ok(());
        }

        let pool = self.get_pool()?;

        let wal_level: String = sqlx::query_scalar("SHOW wal_level")
            .fetch_one(pool)
            .await
            .map_err(|e| Error::InitError(format!("Failed to check WAL level: {e}")))?;

        if wal_level != "logical" {
            return Err(Error::InitError(
                "WAL level must be 'logical' for CDC. Please set wal_level = logical in postgresql.conf".to_string()
            ));
        }

        let publication_name = self
            .config
            .publication_name
            .as_deref()
            .unwrap_or("iggy_publication");
        let tables_clause = if self.config.tables.is_empty() {
            "FOR ALL TABLES".to_string()
        } else {
            format!("FOR TABLE {}", self.config.tables.join(", "))
        };

        let create_publication_sql =
            format!("CREATE PUBLICATION IF NOT EXISTS {publication_name} {tables_clause}");

        sqlx::query(&create_publication_sql)
            .execute(pool)
            .await
            .map_err(|e| Error::InitError(format!("Failed to create publication: {e}")))?;

        let slot_name = self
            .config
            .replication_slot
            .as_deref()
            .unwrap_or("iggy_slot");
        let create_slot_sql = format!(
            "SELECT pg_create_logical_replication_slot('{slot_name}', 'pgoutput') WHERE NOT EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = '{slot_name}')"
        );

        sqlx::query(&create_slot_sql)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::InitError(format!("Failed to create replication slot: {e}")))?;

        info!("PostgreSQL CDC setup completed. Publication: {publication_name}, Slot: {slot_name}");
        Ok(())
    }

    async fn poll_cdc(&self) -> Result<Vec<ProducedMessage>, Error> {
        let backend = self.config.cdc_backend.as_deref().unwrap_or("builtin");
        match backend {
            "builtin" => self.poll_cdc_builtin().await,
            "pg_replicate" => {
                #[cfg(feature = "cdc_pg_replicate")]
                {
                    Err(Error::InitError(
                        "pg_replicate backend not yet implemented".to_string(),
                    ))
                }
                #[cfg(not(feature = "cdc_pg_replicate"))]
                {
                    Err(Error::InitError(
                        "cdc_backend 'pg_replicate' requested but feature 'cdc_pg_replicate' is not enabled at build time".to_string(),
                    ))
                }
            }
            other => Err(Error::InitError(format!(
                "Unsupported cdc_backend '{other}'. Use 'builtin' or 'pg_replicate'"
            ))),
        }
    }

    async fn poll_cdc_builtin(&self) -> Result<Vec<ProducedMessage>, Error> {
        let pool = self.get_pool()?;

        let slot_name = self
            .config
            .replication_slot
            .as_deref()
            .unwrap_or("iggy_slot");
        let publication_name = self
            .config
            .publication_name
            .as_deref()
            .unwrap_or("iggy_publication");
        let capture_ops = self
            .config
            .capture_operations
            .as_ref()
            .map(|ops| ops.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["INSERT", "UPDATE", "DELETE"]);

        let logical_repl_sql = format!(
            "SELECT lsn, xid, data FROM pg_logical_slot_get_changes('{slot_name}', NULL, NULL, 'proto_version', '1', 'publication_names', '{publication_name}')"
        );

        // Database I/O without holding the lock
        let rows = sqlx::query(&logical_repl_sql)
            .fetch_all(pool)
            .await
            .map_err(|e| {
                error!("Failed to fetch CDC changes: {e}");
                Error::InvalidRecord
            })?;

        let mut messages = Vec::new();

        for row in rows {
            let data: String = row.try_get("data").map_err(|_| Error::InvalidRecord)?;

            if let Some(change_record) = self.parse_logical_replication_message(&data, &capture_ops)
            {
                let payload =
                    simd_json::to_vec(&change_record).map_err(|_| Error::InvalidRecord)?;

                let message = ProducedMessage {
                    id: Some(Uuid::new_v4().as_u128()),
                    headers: None,
                    checksum: None,
                    timestamp: Some(Utc::now().timestamp_millis() as u64),
                    origin_timestamp: Some(Utc::now().timestamp_millis() as u64),
                    payload,
                };

                messages.push(message);
            }
        }

        // Update state with minimal lock time
        if !messages.is_empty() {
            let mut state = self.state.lock().await;
            state.processed_rows += messages.len() as u64;
        }

        if self.verbose {
            info!("CDC: Fetched {} change records", messages.len());
        } else {
            debug!("CDC: Fetched {} change records", messages.len());
        }
        Ok(messages)
    }

    async fn poll_tables(&self) -> Result<Vec<ProducedMessage>, Error> {
        if self.config.parallel_tables.unwrap_or(false) {
            self.poll_tables_parallel().await
        } else {
            self.poll_tables_sequential().await
        }
    }

    async fn poll_tables_sequential(&self) -> Result<Vec<ProducedMessage>, Error> {
        let pool = self.get_pool()?;
        let mut messages = Vec::new();

        let batch_size = self.config.batch_size.unwrap_or(1000);
        let tracking_column = self.config.tracking_column.as_deref().unwrap_or("id");
        let pk_column = self
            .config
            .primary_key_column
            .as_deref()
            .unwrap_or(tracking_column);
        let payload_format = self.payload_format();
        let payload_col = self.config.payload_column.as_deref().unwrap_or("");

        let mut state_updates: Vec<(String, String)> = Vec::new();
        let mut total_processed: u64 = 0;

        for table in &self.config.tables {
            let last_offset = {
                let state = self.state.lock().await;
                state.tracking_offsets.get(table).cloned()
            };

            let query = if let Some(custom_query) = &self.config.custom_query {
                self.validate_custom_query(custom_query)?;
                self.substitute_query_params(custom_query, table, &last_offset, batch_size)
            } else {
                self.build_polling_query(table, tracking_column, &last_offset, batch_size)?
            };

            let rows = with_retry(
                || sqlx::query(&query).fetch_all(pool),
                self.get_max_retries(),
                self.retry_delay.as_millis() as u64,
            )
            .await?;

            let (table_messages, max_offset, processed_ids) = self.process_rows(
                &rows,
                table,
                tracking_column,
                pk_column,
                payload_format,
                payload_col,
            )?;

            if !processed_ids.is_empty() {
                self.mark_or_delete_processed_rows(pool, table, pk_column, &processed_ids)
                    .await?;
            }

            let count = table_messages.len();
            total_processed += count as u64;
            messages.extend(table_messages);

            if let Some(offset) = max_offset {
                state_updates.push((table.clone(), offset));
            }

            if self.verbose {
                info!("Fetched {count} rows from table '{table}'");
            } else {
                debug!("Fetched {count} rows from table '{table}'");
            }
        }

        {
            let mut state = self.state.lock().await;
            state.processed_rows += total_processed;
            for (table, offset) in state_updates {
                state.tracking_offsets.insert(table, offset);
            }
            state.last_poll_time = Utc::now();
        }

        Ok(messages)
    }

    async fn poll_tables_parallel(&self) -> Result<Vec<ProducedMessage>, Error> {
        let pool = self.get_pool()?.clone();
        let batch_size = self.config.batch_size.unwrap_or(1000);
        let tracking_column = self.config.tracking_column.as_deref().unwrap_or("id").to_string();
        let pk_column = self
            .config
            .primary_key_column
            .as_deref()
            .unwrap_or(&tracking_column)
            .to_string();
        let payload_format = self.payload_format();
        let payload_col = self.config.payload_column.as_deref().unwrap_or("").to_string();
        let snake_case = self.config.snake_case_columns.unwrap_or(false);
        let include_metadata = self.config.include_metadata.unwrap_or(true);
        let table_namespace = self.config.table_namespace.clone();
        let max_retries = self.get_max_retries();
        let retry_delay_ms = self.retry_delay.as_millis() as u64;
        let verbose = self.verbose;
        let chunk_size = self.config.chunk_size;
        let flat_json_output = self.config.flat_json_output.unwrap_or(false);

        let offsets: HashMap<String, Option<String>> = {
            let state = self.state.lock().await;
            self.config
                .tables
                .iter()
                .map(|t| (t.clone(), state.tracking_offsets.get(t).cloned()))
                .collect()
        };

        let tables = self.config.tables.clone();
        let custom_query = self.config.custom_query.clone();
        let initial_offset = self.config.initial_offset.clone();
        let processed_column = self.config.processed_column.clone();
        let delete_after_read = self.config.delete_after_read.unwrap_or(false);

        let mut join_set = tokio::task::JoinSet::new();

        for table in tables {
            let pool = pool.clone();
            let tracking_col = tracking_column.clone();
            let pk_col = pk_column.clone();
            let payload_c = payload_col.clone();
            let last_offset = offsets.get(&table).cloned().flatten();
            let custom_q = custom_query.clone();
            let init_offset = initial_offset.clone();
            let proc_col = processed_column.clone();
            let ns = table_namespace.clone();

            if let Some(cs) = chunk_size {
                join_set.spawn(poll_table_chunked(
                    pool,
                    table,
                    tracking_col,
                    pk_col,
                    payload_c,
                    payload_format,
                    snake_case,
                    include_metadata,
                    ns,
                    last_offset,
                    cs,
                    max_retries,
                    retry_delay_ms,
                    delete_after_read,
                    proc_col,
                    verbose,
                    flat_json_output,
                ));
            } else {
                join_set.spawn(async move {
                    let query = if let Some(ref cq) = custom_q {
                        substitute_query_params_static(cq, &table, &last_offset, batch_size, &init_offset)
                    } else {
                        build_polling_query_static(
                            &table,
                            &tracking_col,
                            &last_offset,
                            batch_size,
                            &init_offset,
                            proc_col.as_deref(),
                        )?
                    };

                    let rows = with_retry(
                        || sqlx::query(&query).fetch_all(&pool),
                        max_retries,
                        retry_delay_ms,
                    )
                    .await?;

                    let mut messages = Vec::with_capacity(rows.len());
                    let mut max_offset: Option<String> = None;
                    let mut processed_ids: Vec<String> = Vec::new();

                    let emit_table_name = match &ns {
                        Some(namespace) => format!("{namespace}.{table}"),
                        None => table.clone(),
                    };

                    for row in &rows {
                        let mut row_pk: Option<String> = None;
                        let mut extracted_payload: Option<Vec<u8>> = None;
                        let mut data = serde_json::Map::new();

                        for (i, column) in row.columns().iter().enumerate() {
                            let column_name = if snake_case {
                                to_snake_case(column.name())
                            } else {
                                column.name().to_string()
                            };

                            if !payload_c.is_empty() && column.name() == payload_c {
                                extracted_payload =
                                    Some(extract_payload_column_static(row, i, payload_format)?);
                                continue;
                            }

                            let value = extract_column_value_static(row, i)?;
                            data.insert(column_name.clone(), value.clone());

                            if column.name() == tracking_col {
                                if let serde_json::Value::String(ref s) = value {
                                    max_offset = Some(s.clone());
                                } else if let serde_json::Value::Number(ref n) = value {
                                    max_offset = Some(n.to_string());
                                }
                            }

                            if column.name() == pk_col {
                                if let serde_json::Value::String(ref s) = value {
                                    row_pk = Some(s.clone());
                                } else if let serde_json::Value::Number(ref n) = value {
                                    row_pk = Some(n.to_string());
                                }
                            }
                        }

                        if let Some(pk) = row_pk {
                            processed_ids.push(pk);
                        }

                        let payload = if let Some(bytes) = extracted_payload {
                            bytes
                        } else if flat_json_output {
                            data.insert(
                                "table_name".to_string(),
                                serde_json::Value::String(emit_table_name.clone()),
                            );
                            simd_json::to_vec(&data).map_err(|_| Error::InvalidRecord)?
                        } else {
                            let record = if include_metadata {
                                DatabaseRecord {
                                    table_name: emit_table_name.clone(),
                                    operation_type: "SELECT".to_string(),
                                    timestamp: Utc::now(),
                                    data: serde_json::Value::Object(data),
                                    old_data: None,
                                }
                            } else {
                                let mut simple_record = serde_json::Map::new();
                                simple_record
                                    .insert("data".to_string(), serde_json::Value::Object(data));
                                DatabaseRecord {
                                    table_name: emit_table_name.clone(),
                                    operation_type: "SELECT".to_string(),
                                    timestamp: Utc::now(),
                                    data: serde_json::Value::Object(simple_record),
                                    old_data: None,
                                }
                            };
                            simd_json::to_vec(&record).map_err(|_| Error::InvalidRecord)?
                        };

                        messages.push(ProducedMessage {
                            id: Some(Uuid::new_v4().as_u128()),
                            headers: None,
                            checksum: None,
                            timestamp: Some(Utc::now().timestamp_millis() as u64),
                            origin_timestamp: Some(Utc::now().timestamp_millis() as u64),
                            payload,
                        });
                    }

                    if !processed_ids.is_empty() && (delete_after_read || proc_col.is_some()) {
                        mark_or_delete_static(
                            &pool,
                            &table,
                            &pk_col,
                            &processed_ids,
                            delete_after_read,
                            proc_col.as_deref(),
                        )
                        .await?;
                    }

                    if verbose {
                        info!("Fetched {} rows from table '{table}'", messages.len());
                    } else {
                        debug!("Fetched {} rows from table '{table}'", messages.len());
                    }

                    Ok::<_, Error>((table, messages, max_offset))
                });
            }
        }

        let mut all_messages = Vec::new();
        let mut state_updates = Vec::new();
        let mut total_processed: u64 = 0;

        while let Some(result) = join_set.join_next().await {
            let (table, msgs, offset) = result.map_err(|e| {
                error!("Table poll task panicked: {e}");
                Error::InvalidRecord
            })??;

            total_processed += msgs.len() as u64;
            all_messages.extend(msgs);
            if let Some(off) = offset {
                state_updates.push((table, off));
            }
        }

        {
            let mut state = self.state.lock().await;
            state.processed_rows += total_processed;
            for (table, offset) in state_updates {
                state.tracking_offsets.insert(table, offset);
            }
            state.last_poll_time = Utc::now();
        }

        Ok(all_messages)
    }

    async fn poll_snapshot_phase(&self) -> Result<ProducedMessages, Error> {
        let pool = self.get_pool()?.clone();
        let tracking_column = self.config.tracking_column.as_deref().unwrap_or("id").to_string();
        let pk_column = self
            .config
            .primary_key_column
            .as_deref()
            .unwrap_or(&tracking_column)
            .to_string();
        let payload_format = self.payload_format();
        let payload_col = self.config.payload_column.as_deref().unwrap_or("").to_string();
        let snake_case = self.config.snake_case_columns.unwrap_or(false);
        let include_metadata = self.config.include_metadata.unwrap_or(true);
        let table_namespace = self.config.table_namespace.clone();
        let max_retries = self.get_max_retries();
        let retry_delay_ms = self.retry_delay.as_millis() as u64;
        let verbose = self.verbose;
        let chunk_size = self.config.chunk_size.unwrap_or(100_000);

        let tables_done: Vec<String> = {
            let state = self.state.lock().await;
            state.snapshot_tables_done.clone()
        };

        let remaining_tables: Vec<String> = self
            .config
            .tables
            .iter()
            .filter(|t| !tables_done.contains(t))
            .cloned()
            .collect();

        if remaining_tables.is_empty() {
            let mut state = self.state.lock().await;
            state.snapshot_completed = true;
            info!("Snapshot phase complete. Switching to live CDC.");
            return Ok(ProducedMessages {
                schema: Schema::Json,
                messages: Vec::new(),
                state: self.serialize_state(&state),
            });
        }

        let table = remaining_tables[0].clone();
        info!(
            "Snapshot phase: reading table '{}' ({}/{} remaining)",
            table,
            remaining_tables.len(),
            self.config.tables.len()
        );

        let flat_json_output = self.config.flat_json_output.unwrap_or(false);
        let (_, messages, max_offset) = poll_table_chunked(
            pool,
            table.clone(),
            tracking_column,
            pk_column,
            payload_col,
            payload_format,
            snake_case,
            include_metadata,
            table_namespace,
            None,
            chunk_size,
            max_retries,
            retry_delay_ms,
            false,
            None,
            verbose,
            flat_json_output,
        )
        .await?;

        let count = messages.len();

        {
            let mut state = self.state.lock().await;
            state.processed_rows += count as u64;
            state.snapshot_tables_done.push(table.clone());
            if let Some(off) = max_offset {
                state.tracking_offsets.insert(table.clone(), off);
            }

            if state.snapshot_tables_done.len() == self.config.tables.len() {
                state.snapshot_completed = true;
                info!("Snapshot phase complete for all {} tables. Total rows: {}. Switching to live CDC.",
                    self.config.tables.len(), state.processed_rows);
            }
        }

        let state = self.state.lock().await;
        let schema = match self.payload_format() {
            PayloadFormat::Bytea => Schema::Raw,
            PayloadFormat::Text => Schema::Text,
            PayloadFormat::JsonDirect | PayloadFormat::Json => Schema::Json,
        };

        Ok(ProducedMessages {
            schema,
            messages,
            state: self.serialize_state(&state),
        })
    }

    async fn mark_or_delete_processed_rows(
        &self,
        pool: &Pool<Postgres>,
        table: &str,
        pk_column: &str,
        ids: &[String],
    ) -> Result<(), Error> {
        if ids.is_empty() {
            return Ok(());
        }

        let quoted_table = quote_identifier(table)?;
        let quoted_pk = quote_identifier(pk_column)?;

        let ids_list = ids
            .iter()
            .map(|id| {
                if id.parse::<i64>().is_ok() {
                    id.clone()
                } else {
                    format!("'{}'", id.replace('\'', "''"))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        if self.config.delete_after_read.unwrap_or(false) {
            let delete_query =
                format!("DELETE FROM {quoted_table} WHERE {quoted_pk} IN ({ids_list})");

            if self.verbose {
                info!("Deleting {} processed rows from '{table}'", ids.len());
            } else {
                debug!("Deleting {} processed rows from '{table}'", ids.len());
            }

            sqlx::query(&delete_query)
                .execute(pool)
                .await
                .map_err(|e| {
                    error!("Failed to delete processed rows: {e}");
                    Error::InvalidRecord
                })?;
        } else if let Some(processed_col) = &self.config.processed_column {
            let quoted_processed = quote_identifier(processed_col)?;
            let update_query = format!(
                "UPDATE {quoted_table} SET {quoted_processed} = TRUE WHERE {quoted_pk} IN ({ids_list})"
            );

            if self.verbose {
                info!("Marking {} rows as processed in '{table}'", ids.len());
            } else {
                debug!("Marking {} rows as processed in '{table}'", ids.len());
            }

            sqlx::query(&update_query)
                .execute(pool)
                .await
                .map_err(|e| {
                    error!("Failed to mark rows as processed: {e}");
                    Error::InvalidRecord
                })?;
        }

        Ok(())
    }

    fn get_pool(&self) -> Result<&Pool<Postgres>, Error> {
        self.pool
            .as_ref()
            .ok_or_else(|| Error::InitError("Database not connected".to_string()))
    }

    fn payload_format(&self) -> PayloadFormat {
        if let Some(ref payload_col) = self.config.payload_column
            && !payload_col.is_empty()
        {
            return PayloadFormat::from_config(self.config.payload_format.as_deref());
        }
        PayloadFormat::Json
    }

    fn emit_table_name(&self, table: &str) -> String {
        match &self.config.table_namespace {
            Some(ns) => format!("{ns}.{table}"),
            None => table.to_string(),
        }
    }

    fn process_rows(
        &self,
        rows: &[sqlx::postgres::PgRow],
        table: &str,
        tracking_column: &str,
        pk_column: &str,
        payload_format: PayloadFormat,
        payload_col: &str,
    ) -> Result<(Vec<ProducedMessage>, Option<String>, Vec<String>), Error> {
        let mut messages = Vec::with_capacity(rows.len());
        let mut max_offset: Option<String> = None;
        let mut processed_ids: Vec<String> = Vec::new();
        let emit_name = self.emit_table_name(table);

        for row in rows {
            let mut row_pk: Option<String> = None;
            let mut extracted_payload: Option<Vec<u8>> = None;
            let mut data = serde_json::Map::new();

            for (i, column) in row.columns().iter().enumerate() {
                let column_name = if self.config.snake_case_columns.unwrap_or(false) {
                    to_snake_case(column.name())
                } else {
                    column.name().to_string()
                };

                if !payload_col.is_empty() && column.name() == payload_col {
                    extracted_payload =
                        Some(self.extract_payload_column(row, i, payload_format)?);
                    continue;
                }

                let value = self.extract_column_value(row, i)?;
                data.insert(column_name.clone(), value.clone());

                if column.name() == tracking_column {
                    if let serde_json::Value::String(ref s) = value {
                        max_offset = Some(s.clone());
                    } else if let serde_json::Value::Number(ref n) = value {
                        max_offset = Some(n.to_string());
                    }
                }

                if column.name() == pk_column {
                    if let serde_json::Value::String(ref s) = value {
                        row_pk = Some(s.clone());
                    } else if let serde_json::Value::Number(ref n) = value {
                        row_pk = Some(n.to_string());
                    }
                }
            }

            if let Some(pk) = row_pk {
                processed_ids.push(pk);
            }

            let payload = if let Some(bytes) = extracted_payload {
                bytes
            } else if self.config.flat_json_output.unwrap_or(false) {
                data.insert(
                    "table_name".to_string(),
                    serde_json::Value::String(emit_name.clone()),
                );
                simd_json::to_vec(&data).map_err(|_| Error::InvalidRecord)?
            } else {
                let record = if self.config.include_metadata.unwrap_or(true) {
                    DatabaseRecord {
                        table_name: emit_name.clone(),
                        operation_type: "SELECT".to_string(),
                        timestamp: Utc::now(),
                        data: serde_json::Value::Object(data),
                        old_data: None,
                    }
                } else {
                    let mut simple_record = serde_json::Map::new();
                    simple_record.insert("data".to_string(), serde_json::Value::Object(data));
                    DatabaseRecord {
                        table_name: emit_name.clone(),
                        operation_type: "SELECT".to_string(),
                        timestamp: Utc::now(),
                        data: serde_json::Value::Object(simple_record),
                        old_data: None,
                    }
                };
                simd_json::to_vec(&record).map_err(|_| Error::InvalidRecord)?
            };

            messages.push(ProducedMessage {
                id: Some(Uuid::new_v4().as_u128()),
                headers: None,
                checksum: None,
                timestamp: Some(Utc::now().timestamp_millis() as u64),
                origin_timestamp: Some(Utc::now().timestamp_millis() as u64),
                payload,
            });
        }

        Ok((messages, max_offset, processed_ids))
    }

    fn get_max_retries(&self) -> u32 {
        self.config.max_retries.unwrap_or(DEFAULT_MAX_RETRIES)
    }

    fn build_polling_query(
        &self,
        table: &str,
        tracking_column: &str,
        last_offset: &Option<String>,
        batch_size: u32,
    ) -> Result<String, Error> {
        let quoted_table = quote_identifier(table)?;
        let quoted_tracking = quote_identifier(tracking_column)?;

        let base_query = format!("SELECT * FROM {quoted_table}");

        let mut conditions = Vec::new();

        if let Some(offset) = last_offset {
            conditions.push(format!(
                "{quoted_tracking} > {}",
                format_offset_value(offset)
            ));
        } else if let Some(initial) = &self.config.initial_offset {
            conditions.push(format!(
                "{quoted_tracking} > {}",
                format_offset_value(initial)
            ));
        }

        if let Some(processed_col) = &self.config.processed_column {
            let quoted_processed = quote_identifier(processed_col)?;
            conditions.push(format!("{quoted_processed} = FALSE"));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        let order_clause = format!(" ORDER BY {quoted_tracking} ASC");
        let limit_clause = format!(" LIMIT {batch_size}");

        Ok(format!(
            "{base_query}{where_clause}{order_clause}{limit_clause}"
        ))
    }

    fn validate_custom_query(&self, query: &str) -> Result<(), Error> {
        let query_upper = query.to_uppercase();
        if !query_upper.contains("SELECT") {
            warn!("Custom query should contain SELECT statement");
        }
        if query.contains("$table") && self.config.tables.is_empty() {
            return Err(Error::InvalidConfig);
        }
        Ok(())
    }

    fn substitute_query_params(
        &self,
        query: &str,
        table: &str,
        last_offset: &Option<String>,
        batch_size: u32,
    ) -> String {
        let offset_value = last_offset
            .clone()
            .or_else(|| self.config.initial_offset.clone())
            .unwrap_or_default();

        let now = Utc::now();

        query
            .replace("$table", table)
            .replace("$offset", &offset_value)
            .replace("$limit", &batch_size.to_string())
            .replace("$now", &now.to_rfc3339())
            .replace("$now_unix", &now.timestamp().to_string())
    }

    fn parse_logical_replication_message(
        &self,
        data: &str,
        capture_ops: &[&str],
    ) -> Option<DatabaseRecord> {
        if data.starts_with("BEGIN") || data.starts_with("COMMIT") {
            return None;
        }

        if data.starts_with("INSERT:") && capture_ops.contains(&"INSERT") {
            return self.parse_insert_message(data);
        }

        if data.starts_with("UPDATE:") && capture_ops.contains(&"UPDATE") {
            return self.parse_update_message(data);
        }

        if data.starts_with("DELETE:") && capture_ops.contains(&"DELETE") {
            return self.parse_delete_message(data);
        }

        None
    }

    fn parse_insert_message(&self, data: &str) -> Option<DatabaseRecord> {
        if let Some(table_start) = data.find("table ")
            && let Some(colon_pos) = data[table_start..].find(':')
        {
            let table_part = &data[table_start + 6..table_start + colon_pos];
            let table_name = table_part
                .split('.')
                .next_back()
                .unwrap_or(table_part)
                .to_string();

            let data_part = &data[table_start + colon_pos + 1..];
            let parsed_data = parse_record_data(data_part);

            return Some(DatabaseRecord {
                table_name,
                operation_type: "INSERT".to_string(),
                timestamp: Utc::now(),
                data: serde_json::Value::Object(parsed_data),
                old_data: None,
            });
        }
        None
    }

    fn parse_update_message(&self, data: &str) -> Option<DatabaseRecord> {
        if let Some(table_start) = data.find("table ")
            && let Some(colon_pos) = data[table_start..].find(':')
        {
            let table_part = &data[table_start + 6..table_start + colon_pos];
            let table_name = table_part
                .split('.')
                .next_back()
                .unwrap_or(table_part)
                .to_string();

            let data_part = &data[table_start + colon_pos + 1..];
            let parsed_data = parse_record_data(data_part);

            return Some(DatabaseRecord {
                table_name,
                operation_type: "UPDATE".to_string(),
                timestamp: Utc::now(),
                data: serde_json::Value::Object(parsed_data),
                old_data: None,
            });
        }
        None
    }

    fn parse_delete_message(&self, data: &str) -> Option<DatabaseRecord> {
        if let Some(table_start) = data.find("table ")
            && let Some(colon_pos) = data[table_start..].find(':')
        {
            let table_part = &data[table_start + 6..table_start + colon_pos];
            let table_name = table_part
                .split('.')
                .next_back()
                .unwrap_or(table_part)
                .to_string();

            let data_part = &data[table_start + colon_pos + 1..];
            let parsed_data = parse_record_data(data_part);

            return Some(DatabaseRecord {
                table_name,
                operation_type: "DELETE".to_string(),
                timestamp: Utc::now(),
                data: serde_json::Value::Object(parsed_data),
                old_data: None,
            });
        }
        None
    }

    fn extract_payload_column(
        &self,
        row: &sqlx::postgres::PgRow,
        column_index: usize,
        format: PayloadFormat,
    ) -> Result<Vec<u8>, Error> {
        match format {
            PayloadFormat::Bytea => {
                let bytes: Option<Vec<u8>> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(bytes.unwrap_or_default())
            }
            PayloadFormat::Text => {
                let text: Option<String> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(text.unwrap_or_default().into_bytes())
            }
            PayloadFormat::JsonDirect => {
                let json_value: Option<serde_json::Value> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                simd_json::to_vec(&json_value.unwrap_or(serde_json::Value::Null))
                    .map_err(|_| Error::InvalidRecord)
            }
            PayloadFormat::Json => {
                let bytes: Option<Vec<u8>> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(bytes.unwrap_or_default())
            }
        }
    }

    fn extract_column_value(
        &self,
        row: &sqlx::postgres::PgRow,
        column_index: usize,
    ) -> Result<serde_json::Value, Error> {
        let column = &row.columns()[column_index];
        let type_name = column.type_info().name();

        match type_name {
            "BOOL" => {
                let value: Option<bool> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(serde_json::Value::Bool)
                    .unwrap_or(serde_json::Value::Null))
            }
            "INT2" => {
                let value: Option<i16> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(|v| serde_json::Value::from(v as i64))
                    .unwrap_or(serde_json::Value::Null))
            }
            "INT4" => {
                let value: Option<i32> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(|v| serde_json::Value::from(v as i64))
                    .unwrap_or(serde_json::Value::Null))
            }
            "INT8" => {
                let value: Option<i64> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null))
            }
            "FLOAT4" => {
                let value: Option<f32> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(|v| serde_json::Value::from(v as f64))
                    .unwrap_or(serde_json::Value::Null))
            }
            "FLOAT8" => {
                let value: Option<f64> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null))
            }
            "NUMERIC" => {
                let value: Option<String> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null))
            }
            "VARCHAR" | "TEXT" | "CHAR" => {
                let value: Option<String> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null))
            }
            "TIMESTAMP" | "TIMESTAMPTZ" => {
                let value: Option<DateTime<Utc>> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(|dt| serde_json::Value::String(dt.to_rfc3339()))
                    .unwrap_or(serde_json::Value::Null))
            }
            "UUID" => {
                let value: Option<Uuid> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(|u| serde_json::Value::String(u.to_string()))
                    .unwrap_or(serde_json::Value::Null))
            }
            "JSON" | "JSONB" => {
                let value: Option<serde_json::Value> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value.unwrap_or(serde_json::Value::Null))
            }
            "BYTEA" => {
                let value: Option<Vec<u8>> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(|bytes| {
                        use base64::Engine;
                        serde_json::Value::String(
                            base64::engine::general_purpose::STANDARD.encode(&bytes),
                        )
                    })
                    .unwrap_or(serde_json::Value::Null))
            }
            _ => {
                let value: Option<String> = row
                    .try_get(column_index)
                    .map_err(|_| Error::InvalidRecord)?;
                Ok(value
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null))
            }
        }
    }
}

fn quote_identifier(name: &str) -> Result<String, Error> {
    if name.is_empty() {
        return Err(Error::InvalidConfig);
    }
    if name.contains('\0') {
        return Err(Error::InvalidConfig);
    }
    let escaped = name.replace('"', "\"\"");
    Ok(format!("\"{escaped}\""))
}

fn format_offset_value(value: &str) -> String {
    if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

fn to_snake_case(input: &str) -> String {
    let mut result = String::new();
    let mut prev_was_uppercase = false;

    for (i, ch) in input.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 && !prev_was_uppercase {
                result.push('_');
            }
            if let Some(lowercase_ch) = ch.to_lowercase().next() {
                result.push(lowercase_ch);
            } else {
                result.push(ch);
            }
            prev_was_uppercase = true;
        } else {
            result.push(ch);
            prev_was_uppercase = false;
        }
    }

    result
}

fn parse_record_data(data: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut result = serde_json::Map::new();

    for part in data.split_whitespace() {
        if let Some(bracket_pos) = part.find('[')
            && let Some(_close_bracket) = part.find(']')
            && let Some(colon_pos) = part.find(':')
        {
            let column_name = &part[..bracket_pos];
            let value_str = &part[colon_pos + 1..];

            let cleaned_value = if value_str.starts_with('\'') && value_str.ends_with('\'') {
                &value_str[1..value_str.len() - 1]
            } else {
                value_str
            };

            let value = if let Ok(num) = cleaned_value.parse::<i64>() {
                serde_json::Value::Number(serde_json::Number::from(num))
            } else if let Ok(float) = cleaned_value.parse::<f64>() {
                serde_json::Value::Number(
                    serde_json::Number::from_f64(float).unwrap_or(serde_json::Number::from(0)),
                )
            } else if cleaned_value.eq_ignore_ascii_case("true") {
                serde_json::Value::Bool(true)
            } else if cleaned_value.eq_ignore_ascii_case("false") {
                serde_json::Value::Bool(false)
            } else {
                serde_json::Value::String(cleaned_value.to_string())
            };

            result.insert(column_name.to_string(), value);
        }
    }

    result
}

async fn with_retry<T, F, Fut>(operation: F, max_retries: u32, delay_ms: u64) -> Result<T, Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempts = 0;
    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                attempts += 1;
                if attempts >= max_retries || !is_transient_error(&e) {
                    error!("Database operation failed after {attempts} attempts: {e}");
                    return Err(Error::InvalidRecord);
                }
                warn!(
                    "Transient database error (attempt {attempts}/{max_retries}): {e}. Retrying in {delay_ms}ms..."
                );
                tokio::time::sleep(Duration::from_millis(delay_ms * attempts as u64)).await;
            }
        }
    }
}

fn is_transient_error(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Io(_) => true,
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::PoolClosed => false,
        sqlx::Error::Protocol(_) => false,
        sqlx::Error::Database(db_err) => db_err.code().is_some_and(|code| {
            matches!(
                code.as_ref(),
                "40001" | "40P01" | "57P01" | "57P02" | "57P03" | "08000" | "08003" | "08006"
            )
        }),
        _ => false,
    }
}

fn redact_connection_string(conn_str: &str) -> String {
    if let Some(scheme_end) = conn_str.find("://") {
        let scheme = &conn_str[..scheme_end + 3];
        let rest = &conn_str[scheme_end + 3..];
        let preview: String = rest.chars().take(3).collect();
        return format!("{scheme}{preview}***");
    }
    let preview: String = conn_str.chars().take(3).collect();
    format!("{preview}***")
}

async fn get_table_id_range(
    pool: &Pool<Postgres>,
    table: &str,
    tracking_column: &str,
    last_offset: &Option<String>,
) -> Result<Option<(i64, i64)>, Error> {
    let quoted_table = quote_identifier(table)?;
    let quoted_tracking = quote_identifier(tracking_column)?;

    let where_clause = match last_offset {
        Some(offset) => format!(" WHERE {quoted_tracking} > {}", format_offset_value(offset)),
        None => String::new(),
    };

    let query = format!(
        "SELECT MIN({quoted_tracking})::BIGINT as min_id, MAX({quoted_tracking})::BIGINT as max_id FROM {quoted_table}{where_clause}"
    );

    let row = sqlx::query(&query)
        .fetch_one(pool)
        .await
        .map_err(|e| {
            error!("Failed to get ID range for table {table}: {e}");
            Error::InvalidRecord
        })?;

    let min_id: Option<i64> = row.try_get("min_id").unwrap_or(None);
    let max_id: Option<i64> = row.try_get("max_id").unwrap_or(None);

    match (min_id, max_id) {
        (Some(min), Some(max)) if max >= min => Ok(Some((min, max))),
        _ => Ok(None),
    }
}

fn build_chunk_query(
    table: &str,
    tracking_column: &str,
    range_start: i64,
    range_end: i64,
    processed_column: Option<&str>,
) -> Result<String, Error> {
    let quoted_table = quote_identifier(table)?;
    let quoted_tracking = quote_identifier(tracking_column)?;

    let mut conditions = vec![format!(
        "{quoted_tracking} >= {range_start} AND {quoted_tracking} <= {range_end}"
    )];

    if let Some(proc_col) = processed_column {
        let quoted_processed = quote_identifier(proc_col)?;
        conditions.push(format!("{quoted_processed} = FALSE"));
    }

    Ok(format!(
        "SELECT * FROM {quoted_table} WHERE {} ORDER BY {quoted_tracking} ASC",
        conditions.join(" AND ")
    ))
}

async fn poll_table_chunked(
    pool: Pool<Postgres>,
    table: String,
    tracking_column: String,
    pk_column: String,
    payload_col: String,
    payload_format: PayloadFormat,
    snake_case: bool,
    include_metadata: bool,
    table_namespace: Option<String>,
    last_offset: Option<String>,
    chunk_size: u64,
    max_retries: u32,
    retry_delay_ms: u64,
    delete_after_read: bool,
    processed_column: Option<String>,
    verbose: bool,
    flat_json_output: bool,
) -> Result<(String, Vec<ProducedMessage>, Option<String>), Error> {
    let range = get_table_id_range(&pool, &table, &tracking_column, &last_offset).await?;

    let (min_id, max_id) = match range {
        Some(r) => r,
        None => {
            debug!("No rows to process in table '{table}'");
            return Ok((table, Vec::new(), None));
        }
    };

    let total_rows = (max_id - min_id + 1) as u64;
    let num_chunks = (total_rows + chunk_size - 1) / chunk_size;

    info!(
        "Table '{table}': ID range [{min_id}, {max_id}], splitting into {num_chunks} chunks of {chunk_size}"
    );

    let emit_table_name = match &table_namespace {
        Some(ns) => format!("{ns}.{table}"),
        None => table.clone(),
    };

    let mut chunk_tasks = tokio::task::JoinSet::new();

    for chunk_idx in 0..num_chunks {
        let range_start = min_id + (chunk_idx as i64 * chunk_size as i64);
        let range_end = std::cmp::min(
            range_start + chunk_size as i64 - 1,
            max_id,
        );

        let pool = pool.clone();
        let tbl = table.clone();
        let tc = tracking_column.clone();
        let pc = pk_column.clone();
        let plc = payload_col.clone();
        let etn = emit_table_name.clone();
        let proc_col = processed_column.clone();

        chunk_tasks.spawn(async move {
            let query = build_chunk_query(&tbl, &tc, range_start, range_end, proc_col.as_deref())?;

            let rows = with_retry(
                || sqlx::query(&query).fetch_all(&pool),
                max_retries,
                retry_delay_ms,
            )
            .await?;

            let mut messages = Vec::with_capacity(rows.len());
            let mut max_off: Option<String> = None;
            let mut proc_ids: Vec<String> = Vec::new();

            for row in &rows {
                let mut row_pk: Option<String> = None;
                let mut extracted_payload: Option<Vec<u8>> = None;
                let mut data = serde_json::Map::new();

                for (i, column) in row.columns().iter().enumerate() {
                    let column_name = if snake_case {
                        to_snake_case(column.name())
                    } else {
                        column.name().to_string()
                    };

                    if !plc.is_empty() && column.name() == plc {
                        extracted_payload =
                            Some(extract_payload_column_static(row, i, payload_format)?);
                        continue;
                    }

                    let value = extract_column_value_static(row, i)?;
                    data.insert(column_name.clone(), value.clone());

                    if column.name() == tc {
                        if let serde_json::Value::String(ref s) = value {
                            max_off = Some(s.clone());
                        } else if let serde_json::Value::Number(ref n) = value {
                            max_off = Some(n.to_string());
                        }
                    }
                    if column.name() == pc {
                        if let serde_json::Value::String(ref s) = value {
                            row_pk = Some(s.clone());
                        } else if let serde_json::Value::Number(ref n) = value {
                            row_pk = Some(n.to_string());
                        }
                    }
                }

                if let Some(pk) = row_pk {
                    proc_ids.push(pk);
                }

                let payload = if let Some(bytes) = extracted_payload {
                    bytes
                } else if flat_json_output {
                    data.insert(
                        "table_name".to_string(),
                        serde_json::Value::String(etn.clone()),
                    );
                    simd_json::to_vec(&data).map_err(|_| Error::InvalidRecord)?
                } else {
                    let record = if include_metadata {
                        DatabaseRecord {
                            table_name: etn.clone(),
                            operation_type: "SELECT".to_string(),
                            timestamp: Utc::now(),
                            data: serde_json::Value::Object(data),
                            old_data: None,
                        }
                    } else {
                        let mut simple_record = serde_json::Map::new();
                        simple_record
                            .insert("data".to_string(), serde_json::Value::Object(data));
                        DatabaseRecord {
                            table_name: etn.clone(),
                            operation_type: "SELECT".to_string(),
                            timestamp: Utc::now(),
                            data: serde_json::Value::Object(simple_record),
                            old_data: None,
                        }
                    };
                    simd_json::to_vec(&record).map_err(|_| Error::InvalidRecord)?
                };

                messages.push(ProducedMessage {
                    id: Some(Uuid::new_v4().as_u128()),
                    headers: None,
                    checksum: None,
                    timestamp: Some(Utc::now().timestamp_millis() as u64),
                    origin_timestamp: Some(Utc::now().timestamp_millis() as u64),
                    payload,
                });
            }

            if !proc_ids.is_empty() && (delete_after_read || proc_col.is_some()) {
                mark_or_delete_static(
                    &pool,
                    &tbl,
                    &pc,
                    &proc_ids,
                    delete_after_read,
                    proc_col.as_deref(),
                )
                .await?;
            }

            Ok::<_, Error>((messages, max_off))
        });
    }

    let mut all_messages = Vec::new();
    let mut global_max_offset: Option<String> = None;

    while let Some(result) = chunk_tasks.join_next().await {
        let (msgs, chunk_max) = result.map_err(|e| {
            error!("Chunk task panicked: {e}");
            Error::InvalidRecord
        })??;
        all_messages.extend(msgs);
        if let Some(off) = chunk_max {
            global_max_offset = Some(match global_max_offset {
                Some(prev) => {
                    if off.parse::<i64>().unwrap_or(0) > prev.parse::<i64>().unwrap_or(0) {
                        off
                    } else {
                        prev
                    }
                }
                None => off,
            });
        }
    }

    if verbose {
        info!(
            "Table '{table}': fetched {} rows across {num_chunks} chunks",
            all_messages.len()
        );
    } else {
        debug!(
            "Table '{table}': fetched {} rows across {num_chunks} chunks",
            all_messages.len()
        );
    }

    Ok((table, all_messages, global_max_offset))
}

fn build_polling_query_static(
    table: &str,
    tracking_column: &str,
    last_offset: &Option<String>,
    batch_size: u32,
    initial_offset: &Option<String>,
    processed_column: Option<&str>,
) -> Result<String, Error> {
    let quoted_table = quote_identifier(table)?;
    let quoted_tracking = quote_identifier(tracking_column)?;

    let base_query = format!("SELECT * FROM {quoted_table}");
    let mut conditions = Vec::new();

    if let Some(offset) = last_offset {
        conditions.push(format!(
            "{quoted_tracking} > {}",
            format_offset_value(offset)
        ));
    } else if let Some(initial) = initial_offset {
        conditions.push(format!(
            "{quoted_tracking} > {}",
            format_offset_value(initial)
        ));
    }

    if let Some(proc_col) = processed_column {
        let quoted_processed = quote_identifier(proc_col)?;
        conditions.push(format!("{quoted_processed} = FALSE"));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let order_clause = format!(" ORDER BY {quoted_tracking} ASC");
    let limit_clause = format!(" LIMIT {batch_size}");

    Ok(format!(
        "{base_query}{where_clause}{order_clause}{limit_clause}"
    ))
}

fn substitute_query_params_static(
    query: &str,
    table: &str,
    last_offset: &Option<String>,
    batch_size: u32,
    initial_offset: &Option<String>,
) -> String {
    let offset_value = last_offset
        .clone()
        .or_else(|| initial_offset.clone())
        .unwrap_or_default();

    let now = Utc::now();

    query
        .replace("$table", table)
        .replace("$offset", &offset_value)
        .replace("$limit", &batch_size.to_string())
        .replace("$now", &now.to_rfc3339())
        .replace("$now_unix", &now.timestamp().to_string())
}

fn extract_column_value_static(
    row: &sqlx::postgres::PgRow,
    column_index: usize,
) -> Result<serde_json::Value, Error> {
    let column = &row.columns()[column_index];
    let type_name = column.type_info().name();

    match type_name {
        "BOOL" => {
            let value: Option<bool> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(serde_json::Value::Bool)
                .unwrap_or(serde_json::Value::Null))
        }
        "INT2" => {
            let value: Option<i16> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|v| serde_json::Value::from(v as i64))
                .unwrap_or(serde_json::Value::Null))
        }
        "INT4" => {
            let value: Option<i32> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|v| serde_json::Value::from(v as i64))
                .unwrap_or(serde_json::Value::Null))
        }
        "INT8" => {
            let value: Option<i64> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null))
        }
        "FLOAT4" => {
            let value: Option<f32> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|v| serde_json::Value::from(v as f64))
                .unwrap_or(serde_json::Value::Null))
        }
        "FLOAT8" => {
            let value: Option<f64> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null))
        }
        "NUMERIC" => {
            let value: Option<rust_decimal::Decimal> = row
                .try_get(column_index)
                .map_err(|e| {
                    warn!("Failed to extract NUMERIC column {}: {e}", column.name());
                    Error::InvalidRecord
                })?;
            Ok(value
                .and_then(|d| {
                    use rust_decimal::prelude::ToPrimitive;
                    d.to_f64()
                })
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null))
        }
        "DATE" => {
            let value: Option<chrono::NaiveDate> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|d| serde_json::Value::String(d.to_string()))
                .unwrap_or(serde_json::Value::Null))
        }
        "VARCHAR" | "TEXT" | "CHAR" | "BPCHAR" => {
            let value: Option<String> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null))
        }
        "TIMESTAMP" | "TIMESTAMPTZ" => {
            let value: Option<DateTime<Utc>> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|dt| serde_json::Value::String(dt.to_rfc3339()))
                .unwrap_or(serde_json::Value::Null))
        }
        "UUID" => {
            let value: Option<Uuid> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|u| serde_json::Value::String(u.to_string()))
                .unwrap_or(serde_json::Value::Null))
        }
        "JSON" | "JSONB" => {
            let value: Option<serde_json::Value> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value.unwrap_or(serde_json::Value::Null))
        }
        "BYTEA" => {
            let value: Option<Vec<u8>> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(|bytes| {
                    use base64::Engine;
                    serde_json::Value::String(
                        base64::engine::general_purpose::STANDARD.encode(&bytes),
                    )
                })
                .unwrap_or(serde_json::Value::Null))
        }
        _ => {
            let value: Option<String> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(value
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null))
        }
    }
}

fn extract_payload_column_static(
    row: &sqlx::postgres::PgRow,
    column_index: usize,
    format: PayloadFormat,
) -> Result<Vec<u8>, Error> {
    match format {
        PayloadFormat::Bytea => {
            let bytes: Option<Vec<u8>> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(bytes.unwrap_or_default())
        }
        PayloadFormat::Text => {
            let text: Option<String> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(text.unwrap_or_default().into_bytes())
        }
        PayloadFormat::JsonDirect => {
            let json_value: Option<serde_json::Value> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            simd_json::to_vec(&json_value.unwrap_or(serde_json::Value::Null))
                .map_err(|_| Error::InvalidRecord)
        }
        PayloadFormat::Json => {
            let bytes: Option<Vec<u8>> = row
                .try_get(column_index)
                .map_err(|_| Error::InvalidRecord)?;
            Ok(bytes.unwrap_or_default())
        }
    }
}

async fn mark_or_delete_static(
    pool: &Pool<Postgres>,
    table: &str,
    pk_column: &str,
    ids: &[String],
    delete_after_read: bool,
    processed_column: Option<&str>,
) -> Result<(), Error> {
    if ids.is_empty() {
        return Ok(());
    }

    let quoted_table = quote_identifier(table)?;
    let quoted_pk = quote_identifier(pk_column)?;

    let ids_list = ids
        .iter()
        .map(|id| {
            if id.parse::<i64>().is_ok() {
                id.clone()
            } else {
                format!("'{}'", id.replace('\'', "''"))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    if delete_after_read {
        let query = format!("DELETE FROM {quoted_table} WHERE {quoted_pk} IN ({ids_list})");
        sqlx::query(&query).execute(pool).await.map_err(|e| {
            error!("Failed to delete processed rows: {e}");
            Error::InvalidRecord
        })?;
    } else if let Some(proc_col) = processed_column {
        let quoted_processed = quote_identifier(proc_col)?;
        let query = format!(
            "UPDATE {quoted_table} SET {quoted_processed} = TRUE WHERE {quoted_pk} IN ({ids_list})"
        );
        sqlx::query(&query).execute(pool).await.map_err(|e| {
            error!("Failed to mark rows as processed: {e}");
            Error::InvalidRecord
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PostgresSourceConfig {
        PostgresSourceConfig {
            connection_string: SecretString::from("postgres://localhost/db"),
            mode: "polling".to_string(),
            tables: vec!["users".to_string()],
            poll_interval: Some("5s".to_string()),
            batch_size: Some(500),
            tracking_column: Some("updated_at".to_string()),
            initial_offset: None,
            max_connections: None,
            enable_wal_cdc: None,
            custom_query: None,
            snake_case_columns: None,
            include_metadata: None,
            replication_slot: None,
            publication_name: None,
            capture_operations: None,
            cdc_backend: None,
            delete_after_read: None,
            processed_column: None,
            primary_key_column: None,
            payload_column: None,
            payload_format: None,
            verbose_logging: None,
            max_retries: None,
            retry_delay: None,
            table_namespace: None,
            parallel_tables: None,
            chunk_size: None,
            snapshot_mode: None,
        }
    }

    #[test]
    fn given_last_offset_polling_query_should_be_built() {
        let src = PostgresSource::new(1, test_config(), None);
        let query = src
            .build_polling_query("users", "updated_at", &Some("2024-01-01".to_string()), 500)
            .expect("Failed to build query");
        assert_eq!(
            query,
            "SELECT * FROM \"users\" WHERE \"updated_at\" > '2024-01-01' ORDER BY \"updated_at\" ASC LIMIT 500"
        );
    }

    #[test]
    fn given_initial_offset_polling_query_should_be_built() {
        let mut config = test_config();
        config.tracking_column = Some("id".to_string());
        config.initial_offset = Some("100".to_string());
        let src = PostgresSource::new(1, config, None);
        let query = src
            .build_polling_query("users", "id", &None, 1000)
            .expect("Failed to build query");
        assert_eq!(
            query,
            "SELECT * FROM \"users\" WHERE \"id\" > 100 ORDER BY \"id\" ASC LIMIT 1000"
        );
    }

    #[test]
    fn given_processed_column_polling_query_should_include_filter() {
        let mut config = test_config();
        config.processed_column = Some("is_processed".to_string());
        let src = PostgresSource::new(1, config, None);
        let query = src
            .build_polling_query("events", "id", &None, 100)
            .expect("Failed to build query");
        assert!(query.contains("\"is_processed\" = FALSE"));
    }

    #[test]
    fn given_numeric_offset_should_not_quote_value() {
        let src = PostgresSource::new(1, test_config(), None);
        let query = src
            .build_polling_query("users", "id", &Some("42".to_string()), 100)
            .expect("Failed to build query");
        assert!(query.contains("\"id\" > 42"));
        assert!(!query.contains("'42'"));
    }

    #[test]
    fn given_special_chars_in_identifier_should_escape() {
        let result = quote_identifier("table\"name").expect("Failed to quote");
        assert_eq!(result, "\"table\"\"name\"");
    }

    #[test]
    fn given_empty_identifier_should_fail() {
        let result = quote_identifier("");
        assert!(result.is_err());
    }

    #[test]
    fn given_insert_message_should_parse_correctly() {
        let mut config = test_config();
        config.mode = "cdc".to_string();
        let src = PostgresSource::new(1, config, None);

        let data = "INSERT: table public.users: id[1] name['Alice'] active[true]";
        let rec = src
            .parse_logical_replication_message(data, &["INSERT"])
            .unwrap();
        assert_eq!(rec.table_name, "users");
        assert_eq!(rec.operation_type, "INSERT");
    }

    #[test]
    fn given_update_message_should_parse_correctly() {
        let mut config = test_config();
        config.mode = "cdc".to_string();
        let src = PostgresSource::new(1, config, None);

        let data = "UPDATE: table public.orders: id[42] total[99.5]";
        let rec = src
            .parse_logical_replication_message(data, &["UPDATE"])
            .unwrap();
        assert_eq!(rec.table_name, "orders");
        assert_eq!(rec.operation_type, "UPDATE");
    }

    #[test]
    fn given_delete_message_should_parse_correctly() {
        let mut config = test_config();
        config.mode = "cdc".to_string();
        let src = PostgresSource::new(1, config, None);

        let data = "DELETE: table public.products: id[7]";
        let rec = src
            .parse_logical_replication_message(data, &["DELETE"])
            .unwrap();
        assert_eq!(rec.table_name, "products");
        assert_eq!(rec.operation_type, "DELETE");
    }

    #[test]
    fn given_custom_query_params_should_substitute_correctly() {
        let mut config = test_config();
        config.initial_offset = Some("0".to_string());
        let src = PostgresSource::new(1, config, None);

        let query = "SELECT * FROM $table WHERE id > $offset ORDER BY id LIMIT $limit";
        let result = src.substitute_query_params(query, "events", &Some("100".to_string()), 50);

        assert!(result.contains("FROM events"));
        assert!(result.contains("id > 100"));
        assert!(result.contains("LIMIT 50"));
    }

    #[test]
    fn given_custom_query_with_time_params_should_substitute_correctly() {
        let src = PostgresSource::new(1, test_config(), None);

        let query = "SELECT * FROM $table WHERE created_at < '$now'";
        let result = src.substitute_query_params(query, "logs", &None, 100);

        assert!(result.contains("FROM logs"));
        assert!(!result.contains("$now"));
    }

    #[test]
    fn given_no_last_offset_should_use_initial_offset() {
        let mut config = test_config();
        config.initial_offset = Some("500".to_string());
        let src = PostgresSource::new(1, config, None);

        let query = "SELECT * FROM $table WHERE id > $offset";
        let result = src.substitute_query_params(query, "data", &None, 100);

        assert!(result.contains("id > 500"));
    }

    #[test]
    fn given_connection_string_with_credentials_should_redact() {
        let conn = "postgres://user:password@localhost:5432/db";
        let redacted = redact_connection_string(conn);
        assert_eq!(redacted, "postgres://use***");
    }

    #[test]
    fn given_connection_string_without_scheme_should_redact() {
        let conn = "localhost:5432/db";
        let redacted = redact_connection_string(conn);
        assert_eq!(redacted, "loc***");
    }

    #[test]
    fn given_postgresql_scheme_should_redact() {
        let conn = "postgresql://admin:secret123@db.example.com:5432/mydb";
        let redacted = redact_connection_string(conn);
        assert_eq!(redacted, "postgresql://adm***");
    }

    #[test]
    fn given_persisted_state_should_restore_tracking_offsets() {
        let state = State {
            last_poll_time: Utc::now(),
            tracking_offsets: HashMap::from([
                ("users".to_string(), "100".to_string()),
                ("orders".to_string(), "2024-01-15T10:30:00Z".to_string()),
            ]),
            processed_rows: 500,
            snapshot_completed: false,
            snapshot_tables_done: Vec::new(),
        };

        let connector_state =
            ConnectorState::serialize(&state, "test", 1).expect("Failed to serialize state");

        let src = PostgresSource::new(1, test_config(), Some(connector_state));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let restored = src.state.lock().await;
            assert_eq!(
                restored.tracking_offsets.get("users"),
                Some(&"100".to_string())
            );
            assert_eq!(
                restored.tracking_offsets.get("orders"),
                Some(&"2024-01-15T10:30:00Z".to_string())
            );
            assert_eq!(restored.processed_rows, 500);
        });
    }

    #[test]
    fn given_no_state_should_start_fresh() {
        let src = PostgresSource::new(1, test_config(), None);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let state = src.state.lock().await;
            assert!(state.tracking_offsets.is_empty());
            assert_eq!(state.processed_rows, 0);
        });
    }

    #[test]
    fn given_invalid_state_should_start_fresh() {
        let invalid_state = ConnectorState(b"not valid json".to_vec());
        let src = PostgresSource::new(1, test_config(), Some(invalid_state));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let state = src.state.lock().await;
            assert!(state.tracking_offsets.is_empty());
            assert_eq!(state.processed_rows, 0);
        });
    }

    #[test]
    fn state_should_be_serializable_and_deserializable() {
        let original = State {
            last_poll_time: DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            tracking_offsets: HashMap::from([("table1".to_string(), "42".to_string())]),
            processed_rows: 1000,
            snapshot_completed: false,
            snapshot_tables_done: Vec::new(),
        };

        let connector_state =
            ConnectorState::serialize(&original, "test", 1).expect("Failed to serialize state");
        let deserialized: State = connector_state
            .deserialize("test", 1)
            .expect("Failed to deserialize state");

        assert_eq!(original.last_poll_time, deserialized.last_poll_time);
        assert_eq!(original.tracking_offsets, deserialized.tracking_offsets);
        assert_eq!(original.processed_rows, deserialized.processed_rows);
    }
}
