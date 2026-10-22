#include "MusubiPresence.h"

void UMusubiPresence::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::StatusPresenceEvent, [this](const FMusubiServerMessage& M) {
		if (M.StatusPresenceEvent.IsSet()) Apply(M.StatusPresenceEvent->joins, M.StatusPresenceEvent->leaves);
	});
	Super::Initialize(Collection);
}

void UMusubiPresence::SetStatus(TOptional<FString> Status, TOptional<FMusubiJson> Rich, TOptional<bool> AppearOffline)
{
	if (Status.IsSet()) LastStatus.status = Status;
	if (Rich.IsSet()) LastStatus.rich = Rich;
	if (AppearOffline.IsSet()) LastStatus.appear_offline = AppearOffline;
	bHasStatus = true;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::StatusUpdate;
	FMusubiClientMessageStatusUpdate U;
	U.status = Status;
	U.rich = Rich;
	U.appear_offline = AppearOffline;
	M.StatusUpdate = MoveTemp(U);
	Send(MoveTemp(M));
}

void UMusubiPresence::Follow(const TArray<FMusubiUuid>& AccountIds, TFunction<void(bool, const TArray<FMusubiUserPresence>&)> OnDone)
{
	for (const FMusubiUuid& Id : AccountIds) FollowSet.Add(Id);
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::StatusFollow;
	FMusubiClientMessageStatusFollow F;
	F.account_ids = AccountIds;
	M.StatusFollow = MoveTemp(F);
	TWeakObjectPtr<UMusubiPresence> Weak(this);
	Request(MoveTemp(M), Expect<FMusubiServerMessageStatus>(EMusubiServerMessageTag::Status, &FMusubiServerMessage::Status,
		[Weak, OnDone](const FMusubiServerMessageStatus& S) {
			if (Weak.IsValid()) Weak->Apply(S.presences, {});
			if (OnDone) OnDone(true, S.presences);
		},
		[OnDone](const FMusubiServerEnvelope&) { if (OnDone) OnDone(false, {}); }));
}

void UMusubiPresence::Unfollow(const TArray<FMusubiUuid>& AccountIds)
{
	TArray<FMusubiUserPresence> Gone;
	for (const FMusubiUuid& Id : AccountIds)
	{
		FollowSet.Remove(Id);
		if (TArray<FMusubiUserPresence>* P = Online.Find(Id)) Gone.Append(*P);
		Online.Remove(Id);
	}
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::StatusUnfollow;
	FMusubiClientMessageStatusUnfollow U;
	U.account_ids = AccountIds;
	M.StatusUnfollow = MoveTemp(U);
	Send(MoveTemp(M));
	if (Gone.Num()) OnLeft.Broadcast(Gone);
}

void UMusubiPresence::Apply(const TArray<FMusubiUserPresence>& Joins, const TArray<FMusubiUserPresence>& Leaves)
{
	for (const FMusubiUserPresence& P : Leaves)
	{
		if (TArray<FMusubiUserPresence>* S = Online.Find(P.account_id))
		{
			S->RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
			if (S->Num() == 0) Online.Remove(P.account_id);
		}
		OnPresenceChanged.Broadcast(P.account_id.ToString(), P.username, P.status.Get(TEXT("")), IsOnline(P.account_id));
	}
	for (const FMusubiUserPresence& P : Joins)
	{
		TArray<FMusubiUserPresence>& S = Online.FindOrAdd(P.account_id);
		S.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
		S.Add(P);
		OnPresenceChanged.Broadcast(P.account_id.ToString(), P.username, P.status.Get(TEXT("")), true);
	}
	if (Joins.Num()) OnJoined.Broadcast(Joins);
	if (Leaves.Num()) OnLeft.Broadcast(Leaves);
}

void UMusubiPresence::OnResubscribe()
{
	if (bHasStatus)
	{
		FMusubiClientMessage M;
		M.Tag = EMusubiClientMessageTag::StatusUpdate;
		M.StatusUpdate = LastStatus;
		Send(MoveTemp(M));
	}
	if (FollowSet.Num()) Follow(FollowSet.Array());
}

void UMusubiPresence::OnDisconnected()
{
	// Presence is only meaningful while connected; everyone is unknown until the next `status` reply.
	TArray<FMusubiUserPresence> Gone;
	for (const auto& P : Online) Gone.Append(P.Value);
	Online.Empty();
	if (Gone.Num()) OnLeft.Broadcast(Gone);
}

TArray<FMusubiUuid> UMusubiPresence::ParseIds(const TArray<FString>& Text)
{
	TArray<FMusubiUuid> Out;
	for (const FString& T : Text) { FMusubiUuid U; if (FMusubiUuid::Parse(T, U)) Out.Add(U); }
	return Out;
}

void UMusubiPresence::SetStatusText(const FString& Status, const FString& RichJson, bool bAppearOffline)
{
	SetStatus(Status, RichJson.IsEmpty() ? TOptional<FMusubiJson>() : TOptional<FMusubiJson>(FMusubiJson::FromText(RichJson)), bAppearOffline);
}

void UMusubiPresence::FollowAccounts(const TArray<FString>& AccountIds, FMusubiCallDone OnDone)
{
	Follow(ParseIds(AccountIds), [OnDone](bool bOk, const TArray<FMusubiUserPresence>& P) {
		FMusubiCallResult R;
		R.bOk = bOk;
		if (bOk)
		{
			TArray<TSharedPtr<FJsonValue>> Arr;
			for (const FMusubiUserPresence& X : P) Arr.Add(FMusubiUserPresence::ToJson(X));
			R.PayloadJson = MusubiJson::Serialize(MakeShared<FJsonValueArray>(Arr));
		}
		else { R.Code = TEXT("unavailable"); R.Message = TEXT("status_follow failed"); }
		OnDone.ExecuteIfBound(R);
	});
}

void UMusubiPresence::UnfollowAccounts(const TArray<FString>& AccountIds) { Unfollow(ParseIds(AccountIds)); }

bool UMusubiPresence::IsAccountOnline(const FString& AccountId) const
{
	FMusubiUuid U;
	return FMusubiUuid::Parse(AccountId, U) && IsOnline(U);
}
