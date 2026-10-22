#include "MusubiNotifications.h"
#include "MusubiSubsystem.h"
#include "MusubiClient.h"

void UMusubiNotifications::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::Notification, [this](const FMusubiServerMessage& M) {
		if (M.Notification.IsSet()) Merge(M.Notification.GetValue(), false, true);
	});
	Super::Initialize(Collection);
}

const FMusubiNotification* UMusubiNotifications::Find(const FString& Id) const
{
	return Items.FindByPredicate([&](const FMusubiNotification& N) { return N.Data.id == Id; });
}

int32 UMusubiNotifications::LocalUnread() const
{
	int32 N = 0;
	for (const FMusubiNotification& I : Items) if (!I.bRead) N++;
	return N;
}

void UMusubiNotifications::Merge(const FMusubiServerMessageNotification& N, bool bRead, bool bAnnounce)
{
	FMusubiNotification* Existing = Items.FindByPredicate([&](const FMusubiNotification& X) { return X.Data.id == N.id; });
	if (Existing) { Existing->Data = N; Existing->bRead = Existing->bRead || bRead; }
	else
	{
		FMusubiNotification Item;
		Item.Data = N;
		Item.bRead = bRead;
		int32 At = 0;
		while (At < Items.Num() && Items[At].Data.create_time > N.create_time) At++;
		Items.Insert(Item, At);
		Existing = &Items[At];
		if (!bRead && ServerUnread >= 0 && bAnnounce) ServerUnread++;
	}
	if (bAnnounce)
	{
		OnReceived.Broadcast(*Existing);
		OnNotification.Broadcast(N.id, N.subject, N.content.ToText(), N.code);
	}
	OnChanged.Broadcast();
}

void UMusubiNotifications::Rest(const FString& Verb, const FString& Path, const TSharedPtr<FJsonObject>& Body, TFunction<void(bool, TSharedPtr<FJsonObject>)> OnDone)
{
	UMusubiSubsystem* M = Musubi();
	TSharedPtr<FMusubiClient> Http = M ? M->Client() : nullptr;
	if (!Http.IsValid() || !Http->HasSession()) { if (OnDone) OnDone(false, nullptr); return; }
	Http->Call(Verb, Path, Body, FMusubiJsonResult::CreateLambda([OnDone](bool bOk, TSharedPtr<FJsonObject> R) { if (OnDone) OnDone(bOk, R); }));
}

void UMusubiNotifications::Fetch(const FString& Path, bool bAppend, TFunction<void(bool)> OnDone)
{
	TWeakObjectPtr<UMusubiNotifications> Weak(this);
	Rest(TEXT("GET"), Path, nullptr, [Weak, bAppend, OnDone](bool bOk, TSharedPtr<FJsonObject> R) {
		if (!Weak.IsValid()) return;
		if (bOk && R.IsValid())
		{
			if (!bAppend) Weak->Items.RemoveAll([](const FMusubiNotification& N) { return N.Data.persistent; });
			const TArray<TSharedPtr<FJsonValue>>* List = nullptr;
			if (R->TryGetArrayField(TEXT("notifications"), List))
			{
				for (const TSharedPtr<FJsonValue>& V : *List)
				{
					FMusubiServerMessageNotification N;
					if (!FMusubiServerMessageNotification::FromJson(V, N)) continue;
					bool bRead = false;
					if (V->Type == EJson::Object) V->AsObject()->TryGetBoolField(TEXT("read"), bRead);
					Weak->Merge(N, bRead, false);
				}
			}
			FString C;
			Weak->Cursor = R->TryGetStringField(TEXT("cursor"), C) && !C.IsEmpty() ? TOptional<FString>(C) : TOptional<FString>();
			double U = 0;
			if (R->TryGetNumberField(TEXT("unread_count"), U)) Weak->ServerUnread = (int32)U;
			Weak->OnChanged.Broadcast();
		}
		if (OnDone) OnDone(bOk);
	});
}

void UMusubiNotifications::Backfill(int32 Limit, bool bUnreadOnly, TFunction<void(bool)> OnDone)
{
	Cursor.Reset();
	Fetch(FString::Printf(TEXT("/v1/notifications?limit=%d%s"), FMath::Clamp(Limit, 1, 500), bUnreadOnly ? TEXT("&unread=true") : TEXT("")), false, OnDone);
}

bool UMusubiNotifications::More(TFunction<void(bool)> OnDone)
{
	if (!Cursor.IsSet()) return false;
	Fetch(FString::Printf(TEXT("/v1/notifications?cursor=%s"), *FGenericPlatformHttp::UrlEncode(Cursor.GetValue())), true, OnDone);
	return true;
}

static TSharedPtr<FJsonObject> IdsBody(const TArray<FString>& Ids)
{
	TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
	TArray<TSharedPtr<FJsonValue>> Arr;
	for (const FString& Id : Ids) Arr.Add(MakeShared<FJsonValueString>(Id));
	B->SetArrayField(TEXT("ids"), Arr);
	return B;
}

void UMusubiNotifications::MarkRead(const TArray<FString>& Ids, TFunction<void(bool)> OnDone)
{
	TWeakObjectPtr<UMusubiNotifications> Weak(this);
	Rest(TEXT("POST"), TEXT("/v1/notifications/read"), IdsBody(Ids), [Weak, Ids, OnDone](bool bOk, TSharedPtr<FJsonObject>) {
		if (bOk && Weak.IsValid())
		{
			for (FMusubiNotification& N : Weak->Items) if (!N.bRead && Ids.Contains(N.Data.id)) { N.bRead = true; if (Weak->ServerUnread > 0) Weak->ServerUnread--; }
			Weak->OnChanged.Broadcast();
		}
		if (OnDone) OnDone(bOk);
	});
}

void UMusubiNotifications::MarkAllRead(TFunction<void(bool)> OnDone)
{
	TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
	B->SetBoolField(TEXT("all"), true);
	TWeakObjectPtr<UMusubiNotifications> Weak(this);
	Rest(TEXT("POST"), TEXT("/v1/notifications/read"), B, [Weak, OnDone](bool bOk, TSharedPtr<FJsonObject>) {
		if (bOk && Weak.IsValid())
		{
			for (FMusubiNotification& N : Weak->Items) N.bRead = true;
			Weak->ServerUnread = 0;
			Weak->OnChanged.Broadcast();
		}
		if (OnDone) OnDone(bOk);
	});
}

void UMusubiNotifications::Delete(const TArray<FString>& Ids, TFunction<void(bool)> OnDone)
{
	TWeakObjectPtr<UMusubiNotifications> Weak(this);
	Rest(TEXT("DELETE"), TEXT("/v1/notifications"), IdsBody(Ids), [Weak, Ids, OnDone](bool bOk, TSharedPtr<FJsonObject>) {
		if (bOk && Weak.IsValid())
		{
			Weak->Items.RemoveAll([&](const FMusubiNotification& N) {
				const bool bGone = Ids.Contains(N.Data.id);
				if (bGone && !N.bRead && Weak->ServerUnread > 0) Weak->ServerUnread--;
				return bGone;
			});
			Weak->OnChanged.Broadcast();
		}
		if (OnDone) OnDone(bOk);
	});
}

// ---- Blueprint ----

void UMusubiNotifications::RefreshInbox(FMusubiCallDone OnDone)
{
	Backfill(50, false, [OnDone](bool bOk) {
		FMusubiCallResult R;
		R.bOk = bOk;
		if (!bOk) { R.Code = TEXT("unavailable"); R.Message = TEXT("GET /v1/notifications failed"); }
		OnDone.ExecuteIfBound(R);
	});
}

void UMusubiNotifications::MarkNotificationsRead(const TArray<FString>& Ids) { MarkRead(Ids); }
void UMusubiNotifications::DeleteNotifications(const TArray<FString>& Ids) { Delete(Ids); }

TArray<FString> UMusubiNotifications::NotificationIds() const
{
	TArray<FString> Out;
	for (const FMusubiNotification& N : Items) Out.Add(N.Data.id);
	return Out;
}
