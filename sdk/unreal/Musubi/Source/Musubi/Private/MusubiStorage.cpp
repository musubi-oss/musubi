#include "MusubiStorage.h"

bool FMusubiStorageSubscription::Matches(const FMusubiServerMessageStorageUpdate& U) const
{
	if (Collection != U.collection) return false;
	if (Key.IsSet() && Key.GetValue() != U.key) return false;
	if (OwnerId.IsSet() && !(OwnerId.GetValue() == U.owner_id)) return false;
	return true;
}

FString UMusubiStorage::CacheKey(const FString& Collection, const FString& Key, const FMusubiUuid& OwnerId)
{
	return OwnerId.ToString() + TEXT("/") + Collection + TEXT("/") + Key;
}

void UMusubiStorage::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::StorageUpdate, [this](const FMusubiServerMessage& M) {
		if (!M.StorageUpdate.IsSet()) return;
		const auto& U = M.StorageUpdate.GetValue();
		const FString K = CacheKey(U.collection, U.key, U.owner_id);
		if (U.deleted) Cache.Remove(K); else Cache.Add(K, U);
		OnUpdate.Broadcast(U);
		OnStorageChanged.Broadcast(U.collection, U.key, U.owner_id.ToString(), U.value.IsSet() ? U.value->ToText() : FString(), U.deleted);
	});
	Super::Initialize(Collection);
}

void UMusubiStorage::SendSubscribe(const FMusubiStorageSubscription& S, FMusubiReply OnDone)
{
	FMusubiClientMessageStorageSubscribe Sub;
	Sub.collection = S.Collection; Sub.key = S.Key; Sub.owner_id = S.OwnerId;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::StorageSubscribe;
	M.StorageSubscribe = MoveTemp(Sub);
	Request(MoveTemp(M), MoveTemp(OnDone));
}

void UMusubiStorage::Subscribe(const FString& Collection, TOptional<FString> Key, TOptional<FMusubiUuid> OwnerId, FMusubiReply OnDone)
{
	FMusubiStorageSubscription S{Collection, Key, OwnerId};
	Subs.AddUnique(S);
	SendSubscribe(S, MoveTemp(OnDone));
}

void UMusubiStorage::Unsubscribe(const FString& Collection, TOptional<FString> Key, TOptional<FMusubiUuid> OwnerId)
{
	FMusubiStorageSubscription S{Collection, Key, OwnerId};
	Subs.Remove(S);
	FMusubiClientMessageStorageUnsubscribe U;
	U.collection = Collection; U.key = Key; U.owner_id = OwnerId;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::StorageUnsubscribe;
	M.StorageUnsubscribe = MoveTemp(U);
	Send(MoveTemp(M));
	// Drop mirrored objects nobody listens to any more.
	for (auto It = Cache.CreateIterator(); It; ++It)
		if (!Subs.ContainsByPredicate([&](const FMusubiStorageSubscription& X) { return X.Matches(It->Value); })) It.RemoveCurrent();
}

const FMusubiServerMessageStorageUpdate* UMusubiStorage::Cached(const FString& Collection, const FString& Key, const FMusubiUuid& OwnerId) const
{
	return Cache.Find(CacheKey(Collection, Key, OwnerId));
}

void UMusubiStorage::OnResubscribe()
{
	for (const FMusubiStorageSubscription& S : Subs) SendSubscribe(S, FMusubiReply());
}

// ---- Blueprint ----

void UMusubiStorage::SubscribeCollection(const FString& Collection, const FString& Key, const FString& OwnerId, FMusubiCallDone OnDone)
{
	FMusubiUuid U;
	const bool bOwner = !OwnerId.IsEmpty() && FMusubiUuid::Parse(OwnerId, U);
	Subscribe(Collection, Key.IsEmpty() ? TOptional<FString>() : TOptional<FString>(Key), bOwner ? TOptional<FMusubiUuid>(U) : TOptional<FMusubiUuid>(),
		FMusubiReply::CreateLambda([OnDone](bool bOk, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); }));
}

void UMusubiStorage::UnsubscribeCollection(const FString& Collection, const FString& Key, const FString& OwnerId)
{
	FMusubiUuid U;
	const bool bOwner = !OwnerId.IsEmpty() && FMusubiUuid::Parse(OwnerId, U);
	Unsubscribe(Collection, Key.IsEmpty() ? TOptional<FString>() : TOptional<FString>(Key), bOwner ? TOptional<FMusubiUuid>(U) : TOptional<FMusubiUuid>());
}

FString UMusubiStorage::CachedValueJson(const FString& Collection, const FString& Key, const FString& OwnerId) const
{
	FMusubiUuid U;
	if (!FMusubiUuid::Parse(OwnerId, U)) return FString();
	const FMusubiServerMessageStorageUpdate* C = Cached(Collection, Key, U);
	return C && C->value.IsSet() ? C->value->ToText() : FString();
}
