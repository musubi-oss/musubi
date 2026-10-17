//! Order-book market (plan 05, "маркет с ордербуком"): limit buy/sell orders per (item, currency)
//! matched by price then time with partial fills. Listings (`market.rs`) sell one concrete stack to
//! one buyer; the book is for fungible goods — ore, ammo, crafting mats — where many sellers and
//! buyers meet at a price.
//!
//! The order row is the escrow: a buy order holds `escrow` money through the wallet ledger, a sell
//! order holds `remaining` units taken from the owner's inventory. Every fill is one transaction:
//! money to the seller (minus the fee), goods to the buyer, both rows advanced, one `market_fills`
//! row — a crash between statements can neither duplicate nor lose anything. Fills always happen
//! at the resting (maker) order's price; a buyer who bid more than the ask gets the difference back.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::market::{check_amount, fee_for, MarketRules};
use crate::EconomyService;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    fn parse(s: &str) -> Result<Side> {
        match s {
            "buy" => Ok(Side::Buy),
            "sell" => Ok(Side::Sell),
            other => Err(Error::internal(format!("bad order side {other}"))),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewOrder {
    pub item_id: String,
    pub currency: String,
    pub side: Side,
    /// Limit price per unit.
    pub price: i64,
    pub quantity: i64,
    /// How long the unfilled remainder rests, seconds (default `default_listing_secs`, capped by
    /// `max_listing_secs`).
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Order {
    pub id: Uuid,
    pub account_id: AccountId,
    pub item_id: String,
    pub currency: String,
    pub side: Side,
    pub price: i64,
    pub quantity: i64,
    pub remaining: i64,
    /// Money still held for a buy order (`price × remaining`); 0 for sell orders.
    pub escrow: i64,
    /// open | filled | cancelled | expired
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub closed_at: Option<OffsetDateTime>,
}

type OrderRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    String,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

const ORDER_COLS: &str =
    "id, account_id, item_id, currency, side, price, quantity, remaining, escrow, status, created_at, expires_at, closed_at";

impl TryFrom<OrderRow> for Order {
    type Error = Error;
    fn try_from(r: OrderRow) -> Result<Self> {
        Ok(Order {
            id: r.0,
            account_id: AccountId(r.1),
            item_id: r.2,
            currency: r.3,
            side: Side::parse(&r.4)?,
            price: r.5,
            quantity: r.6,
            remaining: r.7,
            escrow: r.8,
            status: r.9,
            created_at: r.10,
            expires_at: r.11,
            closed_at: r.12,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Fill {
    pub id: Uuid,
    pub item_id: String,
    pub currency: String,
    pub buy_order_id: Uuid,
    pub sell_order_id: Uuid,
    pub buyer_id: AccountId,
    pub seller_id: AccountId,
    pub quantity: i64,
    /// Per-unit price the fill happened at (the resting order's price).
    pub price: i64,
    /// Fee taken from the seller's proceeds (`price × quantity × fee_bps`).
    pub fee: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type FillRow = (
    Uuid,
    String,
    String,
    Uuid,
    Uuid,
    Uuid,
    Uuid,
    i64,
    i64,
    i64,
    OffsetDateTime,
);

const FILL_COLS: &str = "id, item_id, currency, buy_order_id, sell_order_id, buyer_id, seller_id, quantity, price, fee, created_at";

impl From<FillRow> for Fill {
    fn from(r: FillRow) -> Self {
        Fill {
            id: r.0,
            item_id: r.1,
            currency: r.2,
            buy_order_id: r.3,
            sell_order_id: r.4,
            buyer_id: AccountId(r.5),
            seller_id: AccountId(r.6),
            quantity: r.7,
            price: r.8,
            fee: r.9,
            created_at: r.10,
        }
    }
}

/// Result of placing an order: the order as it rests (or closed, if fully filled) and the fills
/// it produced on the way in.
#[derive(Debug, Clone, Serialize)]
pub struct Placed {
    pub order: Order,
    pub fills: Vec<Fill>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Level {
    pub price: i64,
    pub quantity: i64,
    pub orders: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Book {
    pub item_id: String,
    pub currency: String,
    /// Best (highest) bid first.
    pub bids: Vec<Level>,
    /// Best (lowest) ask first.
    pub asks: Vec<Level>,
    pub last_price: Option<i64>,
    /// Units traded in the last 24 h.
    pub volume_24h: i64,
    pub fills_24h: i64,
}

fn total(price: i64, quantity: i64) -> Result<i64> {
    price
        .checked_mul(quantity)
        .ok_or_else(|| Error::InvalidArgument("price × quantity overflows".into()))
}

impl EconomyService {
    /// Place a limit order: escrow the goods or the money, cross the book while the price allows,
    /// rest the remainder. Own orders are never matched against (no self-trading).
    pub async fn place_order(
        &self,
        me: AccountId,
        n: &NewOrder,
        rules: &MarketRules,
    ) -> Result<Placed> {
        check_amount(n.price, "price")?;
        check_amount(n.quantity, "quantity")?;
        let ttl = n
            .ttl_secs
            .unwrap_or(rules.default_listing_secs)
            .clamp(1, rules.max_listing_secs.max(1));
        let now = OffsetDateTime::now_utc();
        let id = Uuid::now_v7();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let stackable: Option<(bool,)> =
            sqlx::query_as("select stackable from musubi.items where id = $1")
                .bind(&n.item_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        match stackable {
            None => return Err(Error::NotFound("item".into())),
            Some((false,)) => {
                return Err(Error::InvalidArgument(
                    "only stackable items trade on the order book; list unique items instead"
                        .into(),
                ))
            }
            Some((true,)) => {}
        }
        Self::currency_exists_tx(&mut tx, &n.currency).await?;
        let mut escrow = 0;
        match n.side {
            Side::Sell => {
                Self::take_by_item_tx(&mut tx, me, &n.item_id, n.quantity).await?;
            }
            Side::Buy => {
                escrow = total(n.price, n.quantity)?;
                Self::adjust_tx(
                    &mut tx,
                    me,
                    &n.currency,
                    -escrow,
                    "market.order.escrow",
                    &id.to_string(),
                    Some(&format!("market.order.escrow:{id}")),
                    json!({ "order_id": id, "item_id": n.item_id }),
                )
                .await?;
            }
        }
        sqlx::query(
            "insert into musubi.market_orders (id, account_id, item_id, currency, side, price, quantity, remaining, escrow, status, created_at, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, $7, $8, 'open', $9, $10)",
        )
        .bind(id)
        .bind(me.0)
        .bind(&n.item_id)
        .bind(&n.currency)
        .bind(n.side.as_str())
        .bind(n.price)
        .bind(n.quantity)
        .bind(escrow)
        .bind(now)
        .bind(now + time::Duration::seconds(ttl))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;

        let (counter, cmp, order) = match n.side {
            Side::Buy => ("sell", "<=", "asc"),
            Side::Sell => ("buy", ">=", "desc"),
        };
        let sql = format!(
            "select {ORDER_COLS} from musubi.market_orders
             where item_id = $1 and currency = $2 and side = '{counter}' and status = 'open'
               and expires_at > $3 and price {cmp} $4 and account_id <> $5
             order by price {order}, created_at asc for update"
        );
        let makers: Vec<OrderRow> = sqlx::query_as(&sql)
            .bind(&n.item_id)
            .bind(&n.currency)
            .bind(now)
            .bind(n.price)
            .bind(me.0)
            .fetch_all(&mut *tx)
            .await
            .map_err(Error::internal)?;

        let mut remaining = n.quantity;
        let mut fills = Vec::new();
        for row in makers {
            if remaining == 0 {
                break;
            }
            let maker: Order = row.try_into()?;
            let qty = remaining.min(maker.remaining);
            let (buyer, buyer_order, buyer_price, seller, seller_order) = match n.side {
                Side::Buy => (me, id, n.price, maker.account_id, maker.id),
                Side::Sell => (maker.account_id, maker.id, maker.price, me, id),
            };
            let fill = Self::fill_tx(
                &mut tx,
                FillSpec {
                    item_id: &n.item_id,
                    currency: &n.currency,
                    buyer,
                    buyer_order,
                    buyer_price,
                    seller,
                    seller_order,
                    quantity: qty,
                    price: maker.price,
                },
                rules,
                now,
            )
            .await?;
            // Advance the maker; the taker's row is written once after the loop.
            let released = if maker.side == Side::Buy {
                total(maker.price, qty)?
            } else {
                0
            };
            sqlx::query(
                "update musubi.market_orders
                 set remaining = remaining - $2, escrow = escrow - $3,
                     status = case when remaining - $2 = 0 then 'filled' else status end,
                     closed_at = case when remaining - $2 = 0 then $4 else closed_at end
                 where id = $1",
            )
            .bind(maker.id)
            .bind(qty)
            .bind(released)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if n.side == Side::Buy {
                escrow -= total(n.price, qty)?;
            }
            remaining -= qty;
            fills.push(fill);
        }
        let row: OrderRow = sqlx::query_as(&format!(
            "update musubi.market_orders
             set remaining = $2, escrow = $3,
                 status = case when $2 = 0 then 'filled' else 'open' end,
                 closed_at = case when $2 = 0 then $4 else null end
             where id = $1 returning {ORDER_COLS}"
        ))
        .bind(id)
        .bind(remaining)
        .bind(escrow)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Placed {
            order: row.try_into()?,
            fills,
        })
    }

    /// Move money and goods for one fill. The buyer's money is already escrowed at
    /// `buyer_price`; the fill happens at `price` ≤ `buyer_price` and the difference goes back.
    async fn fill_tx(
        tx: &mut Transaction<'_, Postgres>,
        f: FillSpec<'_>,
        rules: &MarketRules,
        now: OffsetDateTime,
    ) -> Result<Fill> {
        let fill_id = Uuid::now_v7();
        let gross = total(f.price, f.quantity)?;
        let fee = fee_for(gross, rules.fee_bps);
        let refund = total(f.buyer_price - f.price, f.quantity)?;
        if refund > 0 {
            Self::adjust_tx(
                tx,
                f.buyer,
                f.currency,
                refund,
                "market.order.refund",
                &fill_id.to_string(),
                Some(&format!("market.order.refund:{fill_id}")),
                json!({ "order_id": f.buyer_order, "fill_id": fill_id }),
            )
            .await?;
        }
        if gross - fee > 0 {
            Self::adjust_tx(
                tx,
                f.seller,
                f.currency,
                gross - fee,
                "market.order.payout",
                &fill_id.to_string(),
                Some(&format!("market.order.payout:{fill_id}")),
                json!({ "order_id": f.seller_order, "fill_id": fill_id, "fee": fee, "buyer_id": f.buyer.0 }),
            )
            .await?;
        }
        Self::grant_item_tx(tx, f.buyer, f.item_id, f.quantity, json!({})).await?;
        let row: FillRow = sqlx::query_as(&format!(
            "insert into musubi.market_fills (id, item_id, currency, buy_order_id, sell_order_id, buyer_id, seller_id, quantity, price, fee, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning {FILL_COLS}"
        ))
        .bind(fill_id)
        .bind(f.item_id)
        .bind(f.currency)
        .bind(f.buyer_order)
        .bind(f.seller_order)
        .bind(f.buyer.0)
        .bind(f.seller.0)
        .bind(f.quantity)
        .bind(f.price)
        .bind(fee)
        .bind(now)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(row.into())
    }

    /// Cancel an open order of mine; the unfilled escrow comes back.
    pub async fn cancel_order(&self, me: AccountId, id: Uuid) -> Result<Order> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let o = Self::lock_order_tx(&mut tx, id).await?;
        if o.account_id != me {
            return Err(Error::PermissionDenied("not your order".into()));
        }
        if o.status != "open" {
            tx.rollback().await.map_err(Error::internal)?;
            return Err(Error::Conflict(format!("order is {}", o.status)));
        }
        Self::release_order_tx(&mut tx, &o, "market.order.refund").await?;
        let o = Self::close_order_tx(&mut tx, id, "cancelled").await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(o)
    }

    /// Return the escrow of every open order past its deadline (the market sweep).
    pub async fn expire_orders(&self, now: OffsetDateTime, limit: i64) -> Result<Vec<Order>> {
        let due: Vec<(Uuid,)> = sqlx::query_as(
            "select id from musubi.market_orders where status = 'open' and expires_at <= $1 order by expires_at limit $2",
        )
        .bind(now)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (id,) in due {
            let mut tx = self.db.begin().await.map_err(Error::internal)?;
            let row: Option<OrderRow> = sqlx::query_as(&format!(
                "select {ORDER_COLS} from musubi.market_orders where id = $1 and status = 'open' and expires_at <= $2
                 for update skip locked"
            ))
            .bind(id)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some(row) = row else { continue };
            let o: Order = row.try_into()?;
            Self::release_order_tx(&mut tx, &o, "market.order.expired").await?;
            let o = Self::close_order_tx(&mut tx, id, "expired").await?;
            tx.commit().await.map_err(Error::internal)?;
            out.push(o);
        }
        Ok(out)
    }

    async fn release_order_tx(
        tx: &mut Transaction<'_, Postgres>,
        o: &Order,
        reason: &str,
    ) -> Result<()> {
        match o.side {
            Side::Buy if o.escrow > 0 => {
                Self::adjust_tx(
                    tx,
                    o.account_id,
                    &o.currency,
                    o.escrow,
                    reason,
                    &o.id.to_string(),
                    Some(&format!("{reason}:{}", o.id)),
                    json!({ "order_id": o.id }),
                )
                .await?;
            }
            Side::Sell if o.remaining > 0 => {
                Self::grant_item_tx(tx, o.account_id, &o.item_id, o.remaining, json!({})).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn lock_order_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Order> {
        let row: Option<OrderRow> = sqlx::query_as(&format!(
            "select {ORDER_COLS} from musubi.market_orders where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        row.ok_or_else(|| Error::NotFound("order".into()))?
            .try_into()
    }

    async fn close_order_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
        status: &str,
    ) -> Result<Order> {
        let row: OrderRow = sqlx::query_as(&format!(
            "update musubi.market_orders set status = $2, escrow = 0, closed_at = now() where id = $1 returning {ORDER_COLS}"
        ))
        .bind(id)
        .bind(status)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        row.try_into()
    }

    pub async fn order(&self, id: Uuid) -> Result<Order> {
        let row: Option<OrderRow> = sqlx::query_as(&format!(
            "select {ORDER_COLS} from musubi.market_orders where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.ok_or_else(|| Error::NotFound("order".into()))?
            .try_into()
    }

    /// My orders, newest first; `status` filters (`open`, `filled`, …), none = all.
    pub async fn my_orders(
        &self,
        me: AccountId,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Order>> {
        let rows: Vec<OrderRow> = sqlx::query_as(&format!(
            "select {ORDER_COLS} from musubi.market_orders
             where account_id = $1 and ($2::text is null or status = $2)
             order by created_at desc limit $3"
        ))
        .bind(me.0)
        .bind(status)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Fills I took part in (either side), newest first.
    pub async fn my_fills(&self, me: AccountId, limit: i64) -> Result<Vec<Fill>> {
        let rows: Vec<FillRow> = sqlx::query_as(&format!(
            "select {FILL_COLS} from musubi.market_fills where buyer_id = $1 or seller_id = $1
             order by created_at desc limit $2"
        ))
        .bind(me.0)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Aggregated depth: open quantity per price level, `depth` levels a side, plus the last
    /// traded price and 24-hour volume.
    pub async fn book(&self, item_id: &str, currency: &str, depth: i64) -> Result<Book> {
        let depth = depth.clamp(1, 200);
        let now = OffsetDateTime::now_utc();
        const BIDS: &str = "select price, sum(remaining)::bigint, count(*)::bigint from musubi.market_orders
             where item_id = $1 and currency = $2 and side = 'buy' and status = 'open' and expires_at > $3
             group by price order by price desc limit $4";
        const ASKS: &str = "select price, sum(remaining)::bigint, count(*)::bigint from musubi.market_orders
             where item_id = $1 and currency = $2 and side = 'sell' and status = 'open' and expires_at > $3
             group by price order by price asc limit $4";
        let mut sides = Vec::with_capacity(2);
        for sql in [BIDS, ASKS] {
            let rows: Vec<(i64, i64, i64)> = sqlx::query_as(sql)
                .bind(item_id)
                .bind(currency)
                .bind(now)
                .bind(depth)
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
            sides.push(rows);
        }
        let asks = sides.pop().unwrap_or_default();
        let bids = sides.pop().unwrap_or_default();
        let last: Option<(i64,)> = sqlx::query_as(
            "select price from musubi.market_fills where item_id = $1 and currency = $2 order by created_at desc limit 1",
        )
        .bind(item_id)
        .bind(currency)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let (volume_24h, fills_24h): (i64, i64) = sqlx::query_as(
            "select coalesce(sum(quantity), 0)::bigint, count(*)::bigint from musubi.market_fills
             where item_id = $1 and currency = $2 and created_at > $3",
        )
        .bind(item_id)
        .bind(currency)
        .bind(now - time::Duration::hours(24))
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let lv = |v: Vec<(i64, i64, i64)>| {
            v.into_iter()
                .map(|(price, quantity, orders)| Level {
                    price,
                    quantity,
                    orders,
                })
                .collect()
        };
        Ok(Book {
            item_id: item_id.into(),
            currency: currency.into(),
            bids: lv(bids),
            asks: lv(asks),
            last_price: last.map(|l| l.0),
            volume_24h,
            fills_24h,
        })
    }
}

struct FillSpec<'a> {
    item_id: &'a str,
    currency: &'a str,
    buyer: AccountId,
    buyer_order: Uuid,
    buyer_price: i64,
    seller: AccountId,
    seller_order: Uuid,
    quantity: i64,
    price: i64,
}
