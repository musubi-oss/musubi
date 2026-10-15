//! Player-to-player trading: an auction house (listings with buyout and/or bids) and direct
//! trade offers. Escrow is physical: offered items leave the inventory into the listing/trade row
//! and bid money is debited through the ledger, so nothing can be double-spent while an offer is
//! open. Every state change is one database transaction.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{EconomyService, InventoryEntry};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EscrowItem {
    pub item_id: String,
    pub quantity: i64,
    #[serde(default)]
    pub instance: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub id: Uuid,
    pub seller_id: AccountId,
    pub item_id: String,
    pub quantity: i64,
    pub instance: Value,
    pub currency: String,
    pub buyout: Option<i64>,
    pub min_bid: i64,
    pub bid: Option<i64>,
    pub bidder_id: Option<AccountId>,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

type ListingRow = (
    Uuid,
    Uuid,
    String,
    i64,
    Value,
    String,
    Option<i64>,
    i64,
    Option<i64>,
    Option<Uuid>,
    String,
    OffsetDateTime,
    OffsetDateTime,
);

const LISTING_COLS: &str = "id, seller_id, item_id, quantity, instance, currency, buyout, min_bid, bid, bidder_id, status, created_at, expires_at";

impl From<ListingRow> for Listing {
    fn from(r: ListingRow) -> Self {
        Listing {
            id: r.0,
            seller_id: AccountId(r.1),
            item_id: r.2,
            quantity: r.3,
            instance: r.4,
            currency: r.5,
            buyout: r.6,
            min_bid: r.7,
            bid: r.8,
            bidder_id: r.9.map(AccountId),
            status: r.10,
            created_at: r.11,
            expires_at: r.12,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewListing {
    pub inventory_id: Uuid,
    #[serde(default = "one")]
    pub quantity: i64,
    pub currency: String,
    #[serde(default)]
    pub buyout: Option<i64>,
    #[serde(default)]
    pub min_bid: i64,
    /// Seconds the listing stays open; `None` = server default.
    #[serde(default)]
    pub duration_secs: Option<i64>,
}

fn one() -> i64 {
    1
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ListingFilter {
    pub item_id: Option<String>,
    pub seller_id: Option<Uuid>,
    pub currency: Option<String>,
    pub cursor: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sale {
    pub id: Uuid,
    pub listing_id: Uuid,
    pub seller_id: AccountId,
    pub buyer_id: AccountId,
    pub item_id: String,
    pub quantity: i64,
    pub currency: String,
    pub price: i64,
    pub fee: i64,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type SaleRow = (
    Uuid,
    Uuid,
    Uuid,
    Uuid,
    String,
    i64,
    String,
    i64,
    i64,
    String,
    OffsetDateTime,
);

const SALE_COLS: &str =
    "id, listing_id, seller_id, buyer_id, item_id, quantity, currency, price, fee, kind, created_at";

impl From<SaleRow> for Sale {
    fn from(r: SaleRow) -> Self {
        Sale {
            id: r.0,
            listing_id: r.1,
            seller_id: AccountId(r.2),
            buyer_id: AccountId(r.3),
            item_id: r.4,
            quantity: r.5,
            currency: r.6,
            price: r.7,
            fee: r.8,
            kind: r.9,
            created_at: r.10,
        }
    }
}

/// What one party puts on the table. Items are concrete escrowed stacks on the offering side and
/// `item_id + quantity` requests on the wanted side; currency is `code → amount`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeSide {
    pub items: Vec<EscrowItem>,
    pub currency: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WantItem {
    pub item_id: String,
    #[serde(default = "one")]
    pub quantity: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewTrade {
    pub to_account: Uuid,
    #[serde(default)]
    pub offer_items: Vec<OfferItem>,
    #[serde(default)]
    pub offer_currency: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub want_items: Vec<WantItem>,
    #[serde(default)]
    pub want_currency: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OfferItem {
    pub inventory_id: Uuid,
    #[serde(default = "one")]
    pub quantity: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trade {
    pub id: Uuid,
    pub from_id: AccountId,
    pub to_id: AccountId,
    pub offer: TradeSide,
    pub want: TradeSide,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

type TradeRow = (
    Uuid,
    Uuid,
    Uuid,
    Value,
    Value,
    String,
    OffsetDateTime,
    OffsetDateTime,
);

const TRADE_COLS: &str = "id, from_id, to_id, offer, want, status, created_at, expires_at";

impl TryFrom<TradeRow> for Trade {
    type Error = Error;
    fn try_from(r: TradeRow) -> Result<Self> {
        Ok(Trade {
            id: r.0,
            from_id: AccountId(r.1),
            to_id: AccountId(r.2),
            offer: serde_json::from_value(r.3).map_err(Error::internal)?,
            want: serde_json::from_value(r.4).map_err(Error::internal)?,
            status: r.5,
            created_at: r.6,
            expires_at: r.7,
        })
    }
}

/// Market rules the service applies; the server fills it from `[market]`.
#[derive(Debug, Clone, Copy)]
pub struct MarketRules {
    pub fee_bps: i64,
    pub max_listing_secs: i64,
    pub default_listing_secs: i64,
    pub trade_ttl_secs: i64,
    pub anti_snipe_secs: i64,
}

impl Default for MarketRules {
    fn default() -> Self {
        Self {
            fee_bps: 500,
            max_listing_secs: 7 * 24 * 3600,
            default_listing_secs: 24 * 3600,
            trade_ttl_secs: 24 * 3600,
            anti_snipe_secs: 60,
        }
    }
}

fn fee_for(price: i64, fee_bps: i64) -> i64 {
    (price.saturating_mul(fee_bps.clamp(0, 10_000))) / 10_000
}

fn check_amount(v: i64, what: &str) -> Result<()> {
    if v <= 0 {
        return Err(Error::InvalidArgument(format!("{what} must be positive")));
    }
    Ok(())
}

/// Outcome of one listing closing, so the HTTP layer can notify the parties.
#[derive(Debug, Clone, Serialize)]
pub struct Settled {
    pub listing: Listing,
    pub sale: Option<Sale>,
}

impl EconomyService {
    /// Remove `quantity` from one specific inventory stack owned by `account` (escrow out).
    pub(crate) async fn take_entry_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        entry_id: Uuid,
        quantity: i64,
    ) -> Result<EscrowItem> {
        check_amount(quantity, "quantity")?;
        let row: Option<(String, i64, Value)> = sqlx::query_as(
            "select item_id, quantity, instance from musubi.inventory where id = $1 and account_id = $2 for update",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, have, instance)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        if have < quantity {
            return Err(Error::ResourceExhausted(format!(
                "have {have} of {item_id}, offering {quantity}"
            )));
        }
        if have == quantity {
            sqlx::query("delete from musubi.inventory where id = $1")
                .bind(entry_id)
                .execute(&mut **tx)
                .await
                .map_err(Error::internal)?;
        } else {
            sqlx::query(
                "update musubi.inventory set quantity = $2, updated_at = now() where id = $1",
            )
            .bind(entry_id)
            .bind(have - quantity)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        }
        Ok(EscrowItem {
            item_id,
            quantity,
            instance,
        })
    }

    /// Take `quantity` of `item_id` from whatever stacks `account` holds (oldest first), keeping
    /// each stack's instance data so unique items re-appear unchanged on the other side.
    pub(crate) async fn take_by_item_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        item_id: &str,
        quantity: i64,
    ) -> Result<Vec<EscrowItem>> {
        check_amount(quantity, "quantity")?;
        let rows: Vec<(Uuid, i64, Value)> = sqlx::query_as(
            "select id, quantity, instance from musubi.inventory where account_id = $1 and item_id = $2
             order by created_at for update",
        )
        .bind(account.0)
        .bind(item_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let have: i64 = rows.iter().map(|r| r.1).sum();
        if have < quantity {
            return Err(Error::ResourceExhausted(format!(
                "have {have} of {item_id}, need {quantity}"
            )));
        }
        let mut left = quantity;
        let mut out = Vec::new();
        for (id, q, instance) in rows {
            if left == 0 {
                break;
            }
            let take = q.min(left);
            left -= take;
            if take == q {
                sqlx::query("delete from musubi.inventory where id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await
                    .map_err(Error::internal)?;
            } else {
                sqlx::query(
                    "update musubi.inventory set quantity = $2, updated_at = now() where id = $1",
                )
                .bind(id)
                .bind(q - take)
                .execute(&mut **tx)
                .await
                .map_err(Error::internal)?;
            }
            out.push(EscrowItem {
                item_id: item_id.into(),
                quantity: take,
                instance,
            });
        }
        Ok(out)
    }

    pub(crate) async fn grant_escrow_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        items: &[EscrowItem],
    ) -> Result<Vec<InventoryEntry>> {
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            out.push(
                Self::grant_item_tx(tx, account, &it.item_id, it.quantity, it.instance.clone())
                    .await?,
            );
        }
        Ok(out)
    }

    async fn currency_exists_tx(tx: &mut Transaction<'_, Postgres>, code: &str) -> Result<()> {
        let c: Option<(String,)> =
            sqlx::query_as("select code from musubi.currencies where code = $1")
                .bind(code)
                .fetch_optional(&mut **tx)
                .await
                .map_err(Error::internal)?;
        if c.is_none() {
            return Err(Error::NotFound(format!("currency {code}")));
        }
        Ok(())
    }

    // ----- listings -----

    pub async fn list_item(
        &self,
        seller: AccountId,
        n: NewListing,
        rules: &MarketRules,
    ) -> Result<Listing> {
        if n.buyout.is_none() && n.min_bid <= 0 {
            return Err(Error::InvalidArgument(
                "a listing needs a buyout price and/or a positive min_bid".into(),
            ));
        }
        if let Some(b) = n.buyout {
            check_amount(b, "buyout")?;
            if n.min_bid > b {
                return Err(Error::InvalidArgument("min_bid above buyout".into()));
            }
        }
        if n.min_bid < 0 {
            return Err(Error::InvalidArgument(
                "min_bid must not be negative".into(),
            ));
        }
        let secs = n.duration_secs.unwrap_or(rules.default_listing_secs);
        if secs <= 0 || secs > rules.max_listing_secs {
            return Err(Error::InvalidArgument(format!(
                "duration_secs must be within 1..={}",
                rules.max_listing_secs
            )));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        Self::currency_exists_tx(&mut tx, &n.currency).await?;
        let escrow = Self::take_entry_tx(&mut tx, seller, n.inventory_id, n.quantity).await?;
        let id = Uuid::now_v7();
        let expires_at = OffsetDateTime::now_utc() + time::Duration::seconds(secs);
        let row: ListingRow = sqlx::query_as(&format!(
            "insert into musubi.market_listings (id, seller_id, item_id, quantity, instance, currency, buyout, min_bid, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning {LISTING_COLS}"
        ))
        .bind(id)
        .bind(seller.0)
        .bind(&escrow.item_id)
        .bind(escrow.quantity)
        .bind(&escrow.instance)
        .bind(&n.currency)
        .bind(n.buyout)
        .bind(n.min_bid)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(row.into())
    }

    /// Open, unexpired listings, newest first; `cursor` = last seen id.
    pub async fn listings(&self, f: &ListingFilter) -> Result<Vec<Listing>> {
        let limit = f.limit.unwrap_or(50).clamp(1, 200);
        let rows: Vec<ListingRow> = sqlx::query_as(&format!(
            "select {LISTING_COLS} from musubi.market_listings
             where status = 'open' and expires_at > now()
               and ($1::text is null or item_id = $1)
               and ($2::uuid is null or seller_id = $2)
               and ($3::text is null or currency = $3)
               and ($4::uuid is null or id < $4)
             order by id desc limit $5"
        ))
        .bind(&f.item_id)
        .bind(f.seller_id)
        .bind(&f.currency)
        .bind(f.cursor)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn listing(&self, id: Uuid) -> Result<Listing> {
        let row: Option<ListingRow> = sqlx::query_as(&format!(
            "select {LISTING_COLS} from musubi.market_listings where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(Into::into)
            .ok_or_else(|| Error::NotFound("listing".into()))
    }

    /// Every listing of `seller` (any status), newest first.
    pub async fn my_listings(&self, seller: AccountId, limit: i64) -> Result<Vec<Listing>> {
        let rows: Vec<ListingRow> = sqlx::query_as(&format!(
            "select {LISTING_COLS} from musubi.market_listings where seller_id = $1 order by id desc limit $2"
        ))
        .bind(seller.0)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn lock_listing_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Listing> {
        let row: Option<ListingRow> = sqlx::query_as(&format!(
            "select {LISTING_COLS} from musubi.market_listings where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        row.map(Into::into)
            .ok_or_else(|| Error::NotFound("listing".into()))
    }

    /// Hand the escrowed item to `buyer`, pay the seller (minus fee), record the sale and close.
    #[allow(clippy::too_many_arguments)]
    async fn close_sold_tx(
        tx: &mut Transaction<'_, Postgres>,
        l: &Listing,
        buyer: AccountId,
        price: i64,
        kind: &str,
        rules: &MarketRules,
        now: OffsetDateTime,
    ) -> Result<Sale> {
        let fee = fee_for(price, rules.fee_bps);
        let payout = price - fee;
        if payout > 0 {
            Self::adjust_tx(
                tx,
                l.seller_id,
                &l.currency,
                payout,
                "market.sale",
                &l.id.to_string(),
                Some(&format!("market.sale:{}", l.id)),
                serde_json::json!({ "listing_id": l.id, "buyer_id": buyer.0, "fee": fee, "kind": kind }),
            )
            .await?;
        }
        Self::grant_item_tx(tx, buyer, &l.item_id, l.quantity, l.instance.clone()).await?;
        let sale_id = Uuid::now_v7();
        let row: SaleRow = sqlx::query_as(&format!(
            "insert into musubi.market_sales (id, listing_id, seller_id, buyer_id, item_id, quantity, currency, price, fee, kind)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) returning {SALE_COLS}"
        ))
        .bind(sale_id)
        .bind(l.id)
        .bind(l.seller_id.0)
        .bind(buyer.0)
        .bind(&l.item_id)
        .bind(l.quantity)
        .bind(&l.currency)
        .bind(price)
        .bind(fee)
        .bind(kind)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "update musubi.market_listings set status = 'sold', bid = $2, bidder_id = $3, closed_at = $4 where id = $1",
        )
        .bind(l.id)
        .bind(price)
        .bind(buyer.0)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(row.into())
    }

    async fn refund_bidder_tx(
        tx: &mut Transaction<'_, Postgres>,
        l: &Listing,
        reason: &str,
    ) -> Result<()> {
        if let (Some(bid), Some(bidder)) = (l.bid, l.bidder_id) {
            Self::adjust_tx(
                tx,
                bidder,
                &l.currency,
                bid,
                reason,
                &l.id.to_string(),
                Some(&format!("{reason}:{}:{bid}", l.id)),
                serde_json::json!({ "listing_id": l.id }),
            )
            .await?;
        }
        Ok(())
    }

    /// Pay the buyout price now. Repeating the call after a successful buy returns the same sale.
    pub async fn buyout(&self, buyer: AccountId, id: Uuid, rules: &MarketRules) -> Result<Sale> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let l = Self::lock_listing_tx(&mut tx, id).await?;
        if l.status == "sold" {
            let prior: Option<SaleRow> = sqlx::query_as(&format!(
                "select {SALE_COLS} from musubi.market_sales where listing_id = $1 and buyer_id = $2 and kind = 'buyout'"
            ))
            .bind(id)
            .bind(buyer.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            return match prior {
                Some(s) => Ok(s.into()),
                None => Err(Error::Conflict("listing already sold".into())),
            };
        }
        let now = OffsetDateTime::now_utc();
        if l.status != "open" || l.expires_at <= now {
            // Release the row lock now: a lazily dropped transaction would make the settler skip
            // this listing (`for update skip locked`).
            tx.rollback().await.map_err(Error::internal)?;
            return Err(Error::Conflict(format!("listing is {}", l.status)));
        }
        if l.seller_id == buyer {
            return Err(Error::PermissionDenied(
                "cannot buy your own listing".into(),
            ));
        }
        let Some(price) = l.buyout else {
            return Err(Error::InvalidArgument("listing has no buyout price".into()));
        };
        Self::adjust_tx(
            &mut tx,
            buyer,
            &l.currency,
            -price,
            "market.buy",
            &id.to_string(),
            Some(&format!("market.buy:{id}")),
            serde_json::json!({ "listing_id": id, "seller_id": l.seller_id.0 }),
        )
        .await?;
        Self::refund_bidder_tx(&mut tx, &l, "market.outbid").await?;
        let sale = Self::close_sold_tx(&mut tx, &l, buyer, price, "buyout", rules, now).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(sale)
    }

    /// Escrow `amount` from the bidder and release the previous bidder's money. A bid at or above
    /// the buyout is treated as a buyout.
    pub async fn bid(
        &self,
        bidder: AccountId,
        id: Uuid,
        amount: i64,
        rules: &MarketRules,
    ) -> Result<Listing> {
        check_amount(amount, "amount")?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let l = Self::lock_listing_tx(&mut tx, id).await?;
        let now = OffsetDateTime::now_utc();
        if l.status != "open" || l.expires_at <= now {
            // Release the row lock now: a lazily dropped transaction would make the settler skip
            // this listing (`for update skip locked`).
            tx.rollback().await.map_err(Error::internal)?;
            return Err(Error::Conflict(format!("listing is {}", l.status)));
        }
        if l.seller_id == bidder {
            return Err(Error::PermissionDenied(
                "cannot bid on your own listing".into(),
            ));
        }
        if l.min_bid <= 0 && l.buyout.is_some() && l.bid.is_none() {
            // buyout-only listing: bids are not accepted below the price
            if Some(amount) < l.buyout {
                return Err(Error::InvalidArgument(
                    "listing is buyout-only; pay the buyout price".into(),
                ));
            }
        }
        if let Some(b) = l.buyout {
            if amount >= b {
                drop(tx);
                self.buyout(bidder, id, rules).await?;
                return self.listing(id).await;
            }
        }
        let floor = match l.bid {
            Some(cur) => cur + 1,
            None => l.min_bid.max(1),
        };
        if amount < floor {
            return Err(Error::InvalidArgument(format!(
                "bid must be at least {floor}"
            )));
        }
        if l.bidder_id == Some(bidder) {
            // raising own bid: only the difference moves
            let diff = amount - l.bid.unwrap_or(0);
            Self::adjust_tx(
                &mut tx,
                bidder,
                &l.currency,
                -diff,
                "market.bid",
                &id.to_string(),
                Some(&format!("market.bid:{id}:{amount}")),
                serde_json::json!({ "listing_id": id, "raise": true }),
            )
            .await?;
        } else {
            Self::adjust_tx(
                &mut tx,
                bidder,
                &l.currency,
                -amount,
                "market.bid",
                &id.to_string(),
                Some(&format!("market.bid:{id}:{amount}")),
                serde_json::json!({ "listing_id": id }),
            )
            .await?;
            Self::refund_bidder_tx(&mut tx, &l, "market.outbid").await?;
        }
        let mut expires_at = l.expires_at;
        let snipe = time::Duration::seconds(rules.anti_snipe_secs.max(0));
        if rules.anti_snipe_secs > 0 && expires_at - now < snipe {
            expires_at = now + snipe;
        }
        let row: ListingRow = sqlx::query_as(&format!(
            "update musubi.market_listings set bid = $2, bidder_id = $3, expires_at = $4 where id = $1 returning {LISTING_COLS}"
        ))
        .bind(id)
        .bind(amount)
        .bind(bidder.0)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(row.into())
    }

    /// Seller withdraws an open listing without bids; the item returns to the inventory.
    pub async fn cancel_listing(&self, seller: AccountId, id: Uuid) -> Result<Listing> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let l = Self::lock_listing_tx(&mut tx, id).await?;
        if l.seller_id != seller {
            return Err(Error::PermissionDenied("not your listing".into()));
        }
        if l.status != "open" {
            return Err(Error::Conflict(format!("listing is {}", l.status)));
        }
        if l.bid.is_some() {
            return Err(Error::Conflict("listing has bids".into()));
        }
        Self::grant_item_tx(&mut tx, seller, &l.item_id, l.quantity, l.instance.clone()).await?;
        let row: ListingRow = sqlx::query_as(&format!(
            "update musubi.market_listings set status = 'cancelled', closed_at = now() where id = $1 returning {LISTING_COLS}"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(row.into())
    }

    /// Close every listing whose deadline has passed: highest bidder wins (seller is paid, fee
    /// taken), otherwise the item goes back to the seller. Each listing is its own transaction;
    /// concurrent settlers skip rows another one holds.
    pub async fn settle_listings(
        &self,
        rules: &MarketRules,
        now: OffsetDateTime,
        limit: i64,
    ) -> Result<Vec<Settled>> {
        let due: Vec<(Uuid,)> = sqlx::query_as(
            "select id from musubi.market_listings where status = 'open' and expires_at <= $1 order by expires_at limit $2",
        )
        .bind(now)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (id,) in due {
            let mut tx = self.db.begin().await.map_err(Error::internal)?;
            let row: Option<ListingRow> = sqlx::query_as(&format!(
                "select {LISTING_COLS} from musubi.market_listings where id = $1 and status = 'open' and expires_at <= $2
                 for update skip locked"
            ))
            .bind(id)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some(row) = row else {
                continue;
            };
            let l: Listing = row.into();
            let sale = match (l.bid, l.bidder_id) {
                (Some(bid), Some(bidder)) => Some(
                    Self::close_sold_tx(&mut tx, &l, bidder, bid, "auction", rules, now).await?,
                ),
                _ => {
                    Self::grant_item_tx(
                        &mut tx,
                        l.seller_id,
                        &l.item_id,
                        l.quantity,
                        l.instance.clone(),
                    )
                    .await?;
                    sqlx::query(
                        "update musubi.market_listings set status = 'expired', closed_at = $2 where id = $1",
                    )
                    .bind(id)
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                    None
                }
            };
            tx.commit().await.map_err(Error::internal)?;
            let listing = self.listing(id).await?;
            out.push(Settled { listing, sale });
        }
        Ok(out)
    }

    pub async fn sales(&self, account: AccountId, limit: i64) -> Result<Vec<Sale>> {
        let rows: Vec<SaleRow> = sqlx::query_as(&format!(
            "select {SALE_COLS} from musubi.market_sales where seller_id = $1 or buyer_id = $1 order by id desc limit $2"
        ))
        .bind(account.0)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    // ----- direct trades -----

    /// Offer items/currency to another player in exchange for items/currency. The offer is
    /// escrowed immediately; the counterpart is only checked when they accept.
    pub async fn propose_trade(
        &self,
        from: AccountId,
        n: NewTrade,
        rules: &MarketRules,
    ) -> Result<Trade> {
        let to = AccountId(n.to_account);
        if to == from {
            return Err(Error::InvalidArgument("cannot trade with yourself".into()));
        }
        if n.offer_items.is_empty()
            && n.offer_currency.is_empty()
            && n.want_items.is_empty()
            && n.want_currency.is_empty()
        {
            return Err(Error::InvalidArgument("empty trade".into()));
        }
        for (c, a) in n.offer_currency.iter().chain(n.want_currency.iter()) {
            check_amount(*a, &format!("currency {c}"))?;
        }
        for w in &n.want_items {
            check_amount(w.quantity, &format!("quantity of {}", w.item_id))?;
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let exists: Option<(Uuid,)> =
            sqlx::query_as("select id from musubi.accounts where id = $1")
                .bind(to.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if exists.is_none() {
            return Err(Error::NotFound("account".into()));
        }
        for c in n.offer_currency.keys().chain(n.want_currency.keys()) {
            Self::currency_exists_tx(&mut tx, c).await?;
        }
        for w in &n.want_items {
            let item: Option<(String,)> =
                sqlx::query_as("select id from musubi.items where id = $1")
                    .bind(&w.item_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
            if item.is_none() {
                return Err(Error::NotFound(format!("item {}", w.item_id)));
            }
        }
        let id = Uuid::now_v7();
        let mut offer = TradeSide {
            items: Vec::new(),
            currency: n.offer_currency.clone(),
        };
        for o in &n.offer_items {
            offer
                .items
                .push(Self::take_entry_tx(&mut tx, from, o.inventory_id, o.quantity).await?);
        }
        for (c, a) in &n.offer_currency {
            Self::adjust_tx(
                &mut tx,
                from,
                c,
                -a,
                "trade.escrow",
                &id.to_string(),
                Some(&format!("trade.escrow:{id}:{c}")),
                serde_json::json!({ "trade_id": id }),
            )
            .await?;
        }
        let want = TradeSide {
            items: n
                .want_items
                .iter()
                .map(|w| EscrowItem {
                    item_id: w.item_id.clone(),
                    quantity: w.quantity,
                    instance: Value::Object(Default::default()),
                })
                .collect(),
            currency: n.want_currency.clone(),
        };
        let expires_at =
            OffsetDateTime::now_utc() + time::Duration::seconds(rules.trade_ttl_secs.max(1));
        let row: TradeRow = sqlx::query_as(&format!(
            "insert into musubi.trades (id, from_id, to_id, offer, want, expires_at) values ($1, $2, $3, $4, $5, $6)
             returning {TRADE_COLS}"
        ))
        .bind(id)
        .bind(from.0)
        .bind(to.0)
        .bind(serde_json::to_value(&offer).map_err(Error::internal)?)
        .bind(serde_json::to_value(&want).map_err(Error::internal)?)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        row.try_into()
    }

    async fn lock_trade_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Trade> {
        let row: Option<TradeRow> = sqlx::query_as(&format!(
            "select {TRADE_COLS} from musubi.trades where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        row.ok_or_else(|| Error::NotFound("trade".into()))?
            .try_into()
    }

    async fn return_offer_tx(
        tx: &mut Transaction<'_, Postgres>,
        t: &Trade,
        reason: &str,
    ) -> Result<()> {
        Self::grant_escrow_tx(tx, t.from_id, &t.offer.items).await?;
        for (c, a) in &t.offer.currency {
            Self::adjust_tx(
                tx,
                t.from_id,
                c,
                *a,
                reason,
                &t.id.to_string(),
                Some(&format!("{reason}:{}:{c}", t.id)),
                serde_json::json!({ "trade_id": t.id }),
            )
            .await?;
        }
        Ok(())
    }

    async fn close_trade_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
        status: &str,
    ) -> Result<Trade> {
        let row: TradeRow = sqlx::query_as(&format!(
            "update musubi.trades set status = $2, closed_at = now() where id = $1 returning {TRADE_COLS}"
        ))
        .bind(id)
        .bind(status)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        row.try_into()
    }

    /// Recipient accepts: their side is taken, both sides are delivered, all in one transaction.
    pub async fn accept_trade(&self, me: AccountId, id: Uuid) -> Result<Trade> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let t = Self::lock_trade_tx(&mut tx, id).await?;
        if t.to_id != me {
            return Err(Error::PermissionDenied("not the recipient".into()));
        }
        if t.status != "pending" {
            return Err(Error::Conflict(format!("trade is {}", t.status)));
        }
        if t.expires_at <= OffsetDateTime::now_utc() {
            return Err(Error::Conflict("trade expired".into()));
        }
        let mut taken = Vec::new();
        for w in &t.want.items {
            taken.extend(Self::take_by_item_tx(&mut tx, me, &w.item_id, w.quantity).await?);
        }
        for (c, a) in &t.want.currency {
            Self::adjust_tx(
                &mut tx,
                me,
                c,
                -a,
                "trade.pay",
                &id.to_string(),
                Some(&format!("trade.pay:{id}:{c}")),
                serde_json::json!({ "trade_id": id, "to": t.from_id.0 }),
            )
            .await?;
            Self::adjust_tx(
                &mut tx,
                t.from_id,
                c,
                *a,
                "trade.receive",
                &id.to_string(),
                Some(&format!("trade.receive:{id}:{c}")),
                serde_json::json!({ "trade_id": id, "from": me.0 }),
            )
            .await?;
        }
        Self::grant_escrow_tx(&mut tx, t.from_id, &taken).await?;
        Self::grant_escrow_tx(&mut tx, me, &t.offer.items).await?;
        for (c, a) in &t.offer.currency {
            Self::adjust_tx(
                &mut tx,
                me,
                c,
                *a,
                "trade.receive",
                &id.to_string(),
                Some(&format!("trade.receive:{id}:{c}")),
                serde_json::json!({ "trade_id": id, "from": t.from_id.0 }),
            )
            .await?;
        }
        let t = Self::close_trade_tx(&mut tx, id, "accepted").await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(t)
    }

    /// Recipient declines or proposer cancels a pending trade; the escrow goes back.
    pub async fn close_trade(&self, me: AccountId, id: Uuid) -> Result<Trade> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let t = Self::lock_trade_tx(&mut tx, id).await?;
        let status = if t.from_id == me {
            "cancelled"
        } else if t.to_id == me {
            "declined"
        } else {
            return Err(Error::PermissionDenied("not your trade".into()));
        };
        if t.status != "pending" {
            return Err(Error::Conflict(format!("trade is {}", t.status)));
        }
        Self::return_offer_tx(&mut tx, &t, "trade.refund").await?;
        let t = Self::close_trade_tx(&mut tx, id, status).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(t)
    }

    /// Return the escrow of every pending trade past its deadline.
    pub async fn expire_trades(&self, now: OffsetDateTime, limit: i64) -> Result<Vec<Trade>> {
        let due: Vec<(Uuid,)> = sqlx::query_as(
            "select id from musubi.trades where status = 'pending' and expires_at <= $1 order by expires_at limit $2",
        )
        .bind(now)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (id,) in due {
            let mut tx = self.db.begin().await.map_err(Error::internal)?;
            let row: Option<TradeRow> = sqlx::query_as(&format!(
                "select {TRADE_COLS} from musubi.trades where id = $1 and status = 'pending' and expires_at <= $2
                 for update skip locked"
            ))
            .bind(id)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some(row) = row else { continue };
            let t: Trade = row.try_into()?;
            Self::return_offer_tx(&mut tx, &t, "trade.refund").await?;
            let t = Self::close_trade_tx(&mut tx, id, "expired").await?;
            tx.commit().await.map_err(Error::internal)?;
            out.push(t);
        }
        Ok(out)
    }

    pub async fn trade(&self, me: AccountId, id: Uuid) -> Result<Trade> {
        let row: Option<TradeRow> = sqlx::query_as(&format!(
            "select {TRADE_COLS} from musubi.trades where id = $1 and (from_id = $2 or to_id = $2)"
        ))
        .bind(id)
        .bind(me.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.ok_or_else(|| Error::NotFound("trade".into()))?
            .try_into()
    }

    /// Trades involving `me`; `pending_only` hides closed ones.
    pub async fn trades(
        &self,
        me: AccountId,
        pending_only: bool,
        limit: i64,
    ) -> Result<Vec<Trade>> {
        let rows: Vec<TradeRow> = sqlx::query_as(&format!(
            "select {TRADE_COLS} from musubi.trades where (from_id = $1 or to_id = $1)
               and (not $2 or (status = 'pending' and expires_at > now()))
             order by id desc limit $3"
        ))
        .bind(me.0)
        .bind(pending_only)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_rounds_down_and_clamps() {
        assert_eq!(fee_for(1000, 500), 50);
        assert_eq!(fee_for(1, 500), 0);
        assert_eq!(fee_for(1000, 20_000), 1000);
        assert_eq!(fee_for(1000, -5), 0);
    }
}
