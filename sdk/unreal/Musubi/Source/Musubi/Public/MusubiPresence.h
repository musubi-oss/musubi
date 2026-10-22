// Presence/status facade: own status, followed accounts, join/leave events (wire tags
// status_update, status_follow/unfollow → status, status_presence_event).
#pragma once

#include "MusubiFacade.h"
#include "MusubiPresence.generated.h"

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPresenceListEvent, const TArray<FMusubiUserPresence>&);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FMusubiOnPresenceChanged, const FString&, AccountId, const FString&, Username, const FString&, Status, bool, bOnline);

UCLASS()
class MUSUBI_API UMusubiPresence : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	/** Set own status; unset optionals keep the current value on the node. Re-sent after every reconnect. */
	void SetStatus(TOptional<FString> Status, TOptional<FMusubiJson> Rich = {}, TOptional<bool> AppearOffline = {});
	/** Follow accounts; the reply lists their current presences (also merged into `Followed`). */
	void Follow(const TArray<FMusubiUuid>& AccountIds, TFunction<void(bool, const TArray<FMusubiUserPresence>&)> OnDone = nullptr);
	void Unfollow(const TArray<FMusubiUuid>& AccountIds);
	/** Last known presence of every followed account that is online (one entry per session). */
	const TMap<FMusubiUuid, TArray<FMusubiUserPresence>>& Followed() const { return Online; }
	bool IsOnline(const FMusubiUuid& AccountId) const { return Online.Contains(AccountId); }

	FMusubiPresenceListEvent OnJoined;
	FMusubiPresenceListEvent OnLeft;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Presence", meta = (AutoCreateRefTerm = "RichJson"))
	void SetStatusText(const FString& Status, const FString& RichJson, bool bAppearOffline = false);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Presence")
	void FollowAccounts(const TArray<FString>& AccountIds, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Presence")
	void UnfollowAccounts(const TArray<FString>& AccountIds);
	UFUNCTION(BlueprintPure, Category = "Musubi|Presence")
	bool IsAccountOnline(const FString& AccountId) const;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Presence") FMusubiOnPresenceChanged OnPresenceChanged;

protected:
	virtual void OnResubscribe() override;
	virtual void OnDisconnected() override;

private:
	FMusubiClientMessageStatusUpdate LastStatus;
	bool bHasStatus = false;
	TSet<FMusubiUuid> FollowSet;
	TMap<FMusubiUuid, TArray<FMusubiUserPresence>> Online;
	void Apply(const TArray<FMusubiUserPresence>& Joins, const TArray<FMusubiUserPresence>& Leaves);
	static TArray<FMusubiUuid> ParseIds(const TArray<FString>& Text);
};
