// Storage facade: live subscriptions to collection/key objects with a local mirror of the last
// pushed value (wire tags storage_subscribe/unsubscribe → storage_update). Reads/writes go over REST.
#pragma once

#include "MusubiFacade.h"
#include "MusubiStorage.generated.h"

struct MUSUBI_API FMusubiStorageSubscription
{
	FString Collection;
	TOptional<FString> Key;
	TOptional<FMusubiUuid> OwnerId;
	bool Matches(const FMusubiServerMessageStorageUpdate& U) const;
	bool operator==(const FMusubiStorageSubscription& O) const { return Collection == O.Collection && Key == O.Key && OwnerId == O.OwnerId; }
};

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiStorageUpdateEvent, const FMusubiServerMessageStorageUpdate&);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FiveParams(FMusubiOnStorageChanged, const FString&, Collection, const FString&, Key, const FString&, OwnerId, const FString&, ValueJson, bool, bDeleted);

UCLASS()
class MUSUBI_API UMusubiStorage : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	/** Subscribe to a whole collection (Key unset), one key, optionally scoped to another owner. Re-sent after reconnect. */
	void Subscribe(const FString& Collection, TOptional<FString> Key = {}, TOptional<FMusubiUuid> OwnerId = {}, FMusubiReply OnDone = FMusubiReply());
	void Unsubscribe(const FString& Collection, TOptional<FString> Key = {}, TOptional<FMusubiUuid> OwnerId = {});
	/** Last pushed object for (owner, collection, key), null if never seen or deleted. */
	const FMusubiServerMessageStorageUpdate* Cached(const FString& Collection, const FString& Key, const FMusubiUuid& OwnerId) const;
	const TArray<FMusubiStorageSubscription>& Subscriptions() const { return Subs; }

	FMusubiStorageUpdateEvent OnUpdate;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Storage")
	void SubscribeCollection(const FString& Collection, const FString& Key, const FString& OwnerId, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Storage")
	void UnsubscribeCollection(const FString& Collection, const FString& Key, const FString& OwnerId);
	UFUNCTION(BlueprintPure, Category = "Musubi|Storage")
	FString CachedValueJson(const FString& Collection, const FString& Key, const FString& OwnerId) const;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Storage") FMusubiOnStorageChanged OnStorageChanged;

protected:
	virtual void OnResubscribe() override;

private:
	TArray<FMusubiStorageSubscription> Subs;
	TMap<FString, FMusubiServerMessageStorageUpdate> Cache;
	static FString CacheKey(const FString& Collection, const FString& Key, const FMusubiUuid& OwnerId);
	void SendSubscribe(const FMusubiStorageSubscription& S, FMusubiReply OnDone);
};
