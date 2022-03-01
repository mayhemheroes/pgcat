use bytes::{Buf, BufMut, BytesMut};
use log::{info, trace};
use tokio::net::tcp::OwnedWriteHalf;

use std::collections::HashMap;

use crate::config::{get_config, parse, Role};
use crate::errors::Error;
use crate::messages::*;
use crate::pool::ConnectionPool;
use crate::stats::get_stats;

/// Handle admin client
pub async fn handle_admin(
    stream: &mut OwnedWriteHalf,
    mut query: BytesMut,
    pool: ConnectionPool,
) -> Result<(), Error> {
    let code = query.get_u8() as char;

    if code != 'Q' {
        return Err(Error::ProtocolSyncError);
    }

    let len = query.get_i32() as usize;
    let query = String::from_utf8_lossy(&query[..len - 5])
        .to_string()
        .to_ascii_uppercase();

    trace!("Admin query: {}", query);

    if query.starts_with("SHOW STATS") {
        trace!("SHOW STATS");
        show_stats(stream).await
    } else if query.starts_with("RELOAD") {
        trace!("RELOAD");
        reload(stream).await
    } else if query.starts_with("SHOW CONFIG") {
        trace!("SHOW CONFIG");
        show_config(stream).await
    } else if query.starts_with("SHOW DATABASES") {
        trace!("SHOW DATABASES");
        show_databases(stream, &pool).await
    } else if query.starts_with("SET ") {
        trace!("SET");
        ignore_set(stream).await
    } else {
        error_response(stream, "Unsupported query against the admin database").await
    }
}

/// SHOW DATABASES
async fn show_databases(stream: &mut OwnedWriteHalf, pool: &ConnectionPool) -> Result<(), Error> {
    let guard = get_config();
    let config = &*guard.clone();
    drop(guard);

    // Columns
    let columns = vec![
        ("name", DataType::Text),
        ("host", DataType::Text),
        ("port", DataType::Text),
        ("database", DataType::Text),
        ("force_user", DataType::Text),
        ("pool_size", DataType::Int4),
        ("min_pool_size", DataType::Int4),
        ("reserve_pool", DataType::Int4),
        ("pool_mode", DataType::Text),
        ("max_connections", DataType::Int4),
        ("current_connections", DataType::Int4),
        ("paused", DataType::Int4),
        ("disabled", DataType::Int4),
    ];

    let mut res = BytesMut::new();

    // RowDescription
    res.put(row_description(&columns));

    for shard in 0..pool.shards() {
        let database_name = &config.shards[&shard.to_string()].database;
        let mut replica_count = 0;

        for server in 0..pool.servers(shard) {
            let address = pool.address(shard, server);
            let name = match address.role {
                Role::Primary => format!("shard_{}_primary", shard),

                Role::Replica => {
                    let name = format!("shard_{}_replica_{}", shard, replica_count);
                    replica_count += 1;
                    name
                }
            };
            let pool_state = pool.pool_state(shard, server);

            res.put(data_row(&vec![
                name,                                 // name
                address.host.to_string(),             // host
                address.port.to_string(),             // port
                database_name.to_string(),            // database
                config.user.name.to_string(),         // force_user
                config.general.pool_size.to_string(), // pool_size
                "0".to_string(),                      // min_pool_size
                "0".to_string(),                      // reserve_pool
                config.general.pool_mode.to_string(), // pool_mode
                config.general.pool_size.to_string(), // max_connections
                pool_state.connections.to_string(),   // current_connections
                "0".to_string(),                      // paused
                "0".to_string(),                      // disabled
            ]));
        }
    }

    res.put(command_complete("SHOW"));

    // ReadyForQuery
    res.put_u8(b'Z');
    res.put_i32(5);
    res.put_u8(b'I');

    write_all_half(stream, res).await
}

/// Ignore any SET commands the client sends.
/// This is common initialization done by ORMs.
async fn ignore_set(stream: &mut OwnedWriteHalf) -> Result<(), Error> {
    custom_protocol_response_ok(stream, "SET").await
}

/// RELOAD
async fn reload(stream: &mut OwnedWriteHalf) -> Result<(), Error> {
    info!("Reloading config");

    let config = get_config();
    let path = config.path.clone().unwrap();

    parse(&path).await?;

    let config = get_config();

    config.show();

    let mut res = BytesMut::new();

    // CommandComplete
    res.put(command_complete("RELOAD"));

    // ReadyForQuery
    res.put_u8(b'Z');
    res.put_i32(5);
    res.put_u8(b'I');

    write_all_half(stream, res).await
}

async fn show_config(stream: &mut OwnedWriteHalf) -> Result<(), Error> {
    let guard = get_config();
    let config = &*guard.clone();
    let config: HashMap<String, String> = config.into();
    drop(guard);

    // Configs that cannot be changed dynamically.
    let immutables = ["host", "port", "connect_timeout"];

    // Columns
    let columns = vec![
        ("key", DataType::Text),
        ("value", DataType::Text),
        ("default", DataType::Text),
        ("changeable", DataType::Text),
    ];

    // Response data
    let mut res = BytesMut::new();
    res.put(row_description(&columns));

    // DataRow rows
    for (key, value) in config {
        let changeable = if immutables.iter().filter(|col| *col == &key).count() == 1 {
            "no".to_string()
        } else {
            "yes".to_string()
        };

        let row = vec![key, value, "-".to_string(), changeable];

        res.put(data_row(&row));
    }

    res.put(command_complete("SHOW"));

    res.put_u8(b'Z');
    res.put_i32(5);
    res.put_u8(b'I');

    write_all_half(stream, res).await
}

/// SHOW STATS
async fn show_stats(stream: &mut OwnedWriteHalf) -> Result<(), Error> {
    let columns = vec![
        ("database", DataType::Text),
        ("total_xact_count", DataType::Numeric),
        ("total_query_count", DataType::Numeric),
        ("total_received", DataType::Numeric),
        ("total_sent", DataType::Numeric),
        ("total_xact_time", DataType::Numeric),
        ("total_query_time", DataType::Numeric),
        ("total_wait_time", DataType::Numeric),
        ("avg_xact_count", DataType::Numeric),
        ("avg_query_count", DataType::Numeric),
        ("avg_recv", DataType::Numeric),
        ("avg_sent", DataType::Numeric),
        ("avg_xact_time", DataType::Numeric),
        ("avg_query_time", DataType::Numeric),
        ("avg_wait_time", DataType::Numeric),
    ];

    let stats = get_stats();
    let mut res = BytesMut::new();
    res.put(row_description(&columns));

    let mut row = vec![
        String::from("all shards"), // TODO: per-database stats,
    ];

    for column in &columns[1..] {
        row.push(stats.get(column.0).unwrap_or(&0).to_string());
    }

    res.put(data_row(&row));
    res.put(command_complete("SHOW"));

    res.put_u8(b'Z');
    res.put_i32(5);
    res.put_u8(b'I');

    write_all_half(stream, res).await
}