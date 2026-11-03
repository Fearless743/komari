//! Client/device repository.

use rusqlite::params;
use rusqlite::types::Value;

use super::{UpdateBuilder, query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::{DbError, Result};
use crate::models::Client;
use crate::time::LocalTime;

/// Mutable subset of [`Client`] fields, used for partial updates. Fields left
/// as `None` are not touched.
#[derive(Clone, Debug, Default)]
pub struct ClientPatch {
    pub name: Option<String>,
    pub token: Option<String>,
    pub cpu_name: Option<String>,
    pub virtualization: Option<String>,
    pub arch: Option<String>,
    pub cpu_cores: Option<i64>,
    pub os: Option<String>,
    pub kernel_version: Option<String>,
    pub gpu_name: Option<String>,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub region: Option<String>,
    pub remark: Option<String>,
    pub public_remark: Option<String>,
    pub mem_total: Option<i64>,
    pub swap_total: Option<i64>,
    pub disk_total: Option<i64>,
    pub version: Option<String>,
    pub weight: Option<i64>,
    pub price: Option<f64>,
    pub billing_cycle: Option<i64>,
    pub auto_renewal: Option<bool>,
    pub currency: Option<String>,
    pub expired_at: Option<LocalTime>,
    pub group: Option<String>,
    pub tags: Option<String>,
    pub ddns_enabled: Option<bool>,
    pub ddns_hostname: Option<String>,
    pub ddns_record_id: Option<String>,
    pub ddns_record_type: Option<String>,
    pub hidden: Option<bool>,
    pub traffic_limit: Option<i64>,
    pub traffic_limit_type: Option<String>,
}

const COLUMNS: &str = "`uuid`, `token`, `name`, `cpu_name`, `virtualization`, `arch`, \
     `cpu_cores`, `os`, `kernel_version`, `gpu_name`, `ipv4`, `ipv6`, `region`, `remark`, \
     `public_remark`, `mem_total`, `swap_total`, `disk_total`, `version`, `weight`, `price`, \
     `billing_cycle`, `auto_renewal`, `currency`, `expired_at`, `group`, `tags`, \
     `ddns_enabled`, `ddns_hostname`, `ddns_record_id`, `ddns_record_type`, `hidden`, \
     `traffic_limit`, `traffic_limit_type`, `created_at`, `updated_at`";

const PLACEHOLDERS: &str = "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, \
     ?32, ?33, ?34, ?35, ?36";

/// Access to the `clients` table.
pub struct Clients<'a> {
    pub(crate) db: &'a Db,
}

impl Clients<'_> {
    /// Insert a client. Use [`Client::new`] to generate UUID/token defaults.
    pub fn create(&self, client: &Client) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            &format!("INSERT INTO clients ({COLUMNS}) VALUES ({PLACEHOLDERS})"),
            params![
                client.uuid,
                client.token,
                client.name,
                client.cpu_name,
                client.virtualization,
                client.arch,
                client.cpu_cores,
                client.os,
                client.kernel_version,
                client.gpu_name,
                client.ipv4,
                client.ipv6,
                client.region,
                client.remark,
                client.public_remark,
                client.mem_total,
                client.swap_total,
                client.disk_total,
                client.version,
                client.weight,
                client.price,
                client.billing_cycle,
                client.auto_renewal,
                client.currency,
                client.expired_at,
                client.group,
                client.tags,
                client.ddns_enabled,
                client.ddns_hostname,
                client.ddns_record_id,
                client.ddns_record_type,
                client.hidden,
                client.traffic_limit,
                client.traffic_limit_type,
                client.created_at,
                client.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Generate and insert a client with the given name (empty name produces
    /// `client_<first 8 UUID chars>`, matching the Go backend).
    pub fn create_named(&self, name: impl Into<String>) -> Result<Client> {
        let client = Client::new(name);
        self.create(&client)?;
        Ok(client)
    }

    /// Look up a client by primary key.
    pub fn get(&self, uuid: &str) -> Result<Option<Client>> {
        let conn = self.db.conn();
        query_optional(&conn, "SELECT * FROM clients WHERE uuid = ?1", [uuid])
    }

    /// Look up a client by its unique token (agent authentication).
    pub fn get_by_token(&self, token: &str) -> Result<Option<Client>> {
        let conn = self.db.conn();
        query_optional(&conn, "SELECT * FROM clients WHERE token = ?1", [token])
    }

    /// Like [`Clients::get`] but returns [`DbError::NotFound`] when absent.
    pub fn require(&self, uuid: &str) -> Result<Client> {
        self.get(uuid)?
            .ok_or_else(|| DbError::not_found("client", uuid))
    }

    /// Every client, in insertion (rowid) order.
    pub fn list(&self) -> Result<Vec<Client>> {
        let conn = self.db.conn();
        query_all(&conn, "SELECT * FROM clients ORDER BY rowid", [])
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM clients", [])
    }

    /// Replace every mutable column with the values in `client`.
    pub fn update(&self, client: &Client) -> Result<usize> {
        let conn = self.db.conn();
        let mut builder = UpdateBuilder::new();
        builder
            .set("token", &client.token)?
            .set("name", &client.name)?
            .set("cpu_name", &client.cpu_name)?
            .set("virtualization", &client.virtualization)?
            .set("arch", &client.arch)?
            .set("cpu_cores", &client.cpu_cores)?
            .set("os", &client.os)?
            .set("kernel_version", &client.kernel_version)?
            .set("gpu_name", &client.gpu_name)?
            .set("ipv4", &client.ipv4)?
            .set("ipv6", &client.ipv6)?
            .set("region", &client.region)?
            .set("remark", &client.remark)?
            .set("public_remark", &client.public_remark)?
            .set("mem_total", &client.mem_total)?
            .set("swap_total", &client.swap_total)?
            .set("disk_total", &client.disk_total)?
            .set("version", &client.version)?
            .set("weight", &client.weight)?
            .set("price", &client.price)?
            .set("billing_cycle", &client.billing_cycle)?
            .set("auto_renewal", &client.auto_renewal)?
            .set("currency", &client.currency)?
            .set("expired_at", &client.expired_at)?
            .set("\"group\"", &client.group)?
            .set("tags", &client.tags)?
            .set("ddns_enabled", &client.ddns_enabled)?
            .set("ddns_hostname", &client.ddns_hostname)?
            .set("ddns_record_id", &client.ddns_record_id)?
            .set("ddns_record_type", &client.ddns_record_type)?
            .set("hidden", &client.hidden)?
            .set("traffic_limit", &client.traffic_limit)?
            .set("traffic_limit_type", &client.traffic_limit_type)?
            .set("created_at", &client.created_at)?
            .set("updated_at", &client.updated_at)?;
        builder.finish(
            &conn,
            "clients",
            "uuid = ?1",
            vec![Value::Text(client.uuid.clone())],
        )
    }

    /// Apply a partial update. `updated_at` is always refreshed, matching the
    /// Go `SaveClient` / `SaveClientInfo` helpers.
    pub fn patch(&self, uuid: &str, patch: &ClientPatch) -> Result<usize> {
        let now = LocalTime::now();
        let mut builder = UpdateBuilder::new();
        builder
            .set_opt("name", patch.name.as_ref())?
            .set_opt("token", patch.token.as_ref())?
            .set_opt("cpu_name", patch.cpu_name.as_ref())?
            .set_opt("virtualization", patch.virtualization.as_ref())?
            .set_opt("arch", patch.arch.as_ref())?
            .set_opt("cpu_cores", patch.cpu_cores.as_ref())?
            .set_opt("os", patch.os.as_ref())?
            .set_opt("kernel_version", patch.kernel_version.as_ref())?
            .set_opt("gpu_name", patch.gpu_name.as_ref())?
            .set_opt("ipv4", patch.ipv4.as_ref())?
            .set_opt("ipv6", patch.ipv6.as_ref())?
            .set_opt("region", patch.region.as_ref())?
            .set_opt("remark", patch.remark.as_ref())?
            .set_opt("public_remark", patch.public_remark.as_ref())?
            .set_opt("mem_total", patch.mem_total.as_ref())?
            .set_opt("swap_total", patch.swap_total.as_ref())?
            .set_opt("disk_total", patch.disk_total.as_ref())?
            .set_opt("version", patch.version.as_ref())?
            .set_opt("weight", patch.weight.as_ref())?
            .set_opt("price", patch.price.as_ref())?
            .set_opt("billing_cycle", patch.billing_cycle.as_ref())?
            .set_opt("auto_renewal", patch.auto_renewal.as_ref())?
            .set_opt("currency", patch.currency.as_ref())?
            .set_opt("expired_at", patch.expired_at.as_ref())?
            .set_opt("\"group\"", patch.group.as_ref())?
            .set_opt("tags", patch.tags.as_ref())?
            .set_opt("ddns_enabled", patch.ddns_enabled.as_ref())?
            .set_opt("ddns_hostname", patch.ddns_hostname.as_ref())?
            .set_opt("ddns_record_id", patch.ddns_record_id.as_ref())?
            .set_opt("ddns_record_type", patch.ddns_record_type.as_ref())?
            .set_opt("hidden", patch.hidden.as_ref())?
            .set_opt("traffic_limit", patch.traffic_limit.as_ref())?
            .set_opt("traffic_limit_type", patch.traffic_limit_type.as_ref())?;
        if builder.is_empty() {
            return Ok(0);
        }
        builder.set("updated_at", &now)?;
        let conn = self.db.conn();
        builder.finish(
            &conn,
            "clients",
            "uuid = ?1",
            vec![Value::Text(uuid.to_owned())],
        )
    }

    pub fn rename(&self, uuid: &str, name: impl Into<String>) -> Result<usize> {
        self.patch(
            uuid,
            &ClientPatch {
                name: Some(name.into()),
                ..ClientPatch::default()
            },
        )
    }

    pub fn set_token(&self, uuid: &str, token: impl Into<String>) -> Result<usize> {
        self.patch(
            uuid,
            &ClientPatch {
                token: Some(token.into()),
                ..ClientPatch::default()
            },
        )
    }

    /// Delete a client. Returns `true` when a row was removed.
    pub fn delete(&self, uuid: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM clients WHERE uuid = ?1", [uuid])? > 0)
    }
}
