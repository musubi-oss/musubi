// Notifications facade: realtime pushes (wire tag notification) merged with the REST inbox
// (GET/DELETE /v1/notifications, POST /v1/notifications/read) so the local list is complete after a reconnect.
#pragma once

#include "MusubiFacade.h"
#include "MusubiNotifications.generated.h"

struct MUSUBI_API FMusubiNotification
{
	FMusubiServerMessageNotification Data;
	bool bRead = false;
};

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiNotificationEvent, const FMusubiNotification&);
DECLARE_MULTICAST_DELEGATE(FMusubiNotificationsChanged);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FMusubiOnNotification, const FString&, Id, const FString&, Subject, const FString&, ContentJson, int32, Code);

UCLASS()
class MUSUBI_API UMusubiNotifications : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	/** Pull the inbox from REST (first page of `Limit`, unread only if asked) and merge it into `All()`. Runs on every (re)connect too. */
	void Backfill(int32 Limit = 50, bool bUnreadOnly = false, TFunction<void(bool)> OnDone = nullptr);
	/** Fetch the next page after the last `Backfill`; false when there is no cursor. */
	bool More(TFunction<void(bool)> OnDone = nullptr);
	void MarkRead(const TArray<FString>& Ids, TFunction<void(bool)> OnDone = nullptr);
	void MarkAllRead(TFunction<void(bool)> OnDone = nullptr);
	void Delete(const TArray<FString>& Ids, TFunction<void(bool)> OnDone = nullptr);

	/** Newest first. Non-persistent pushes are kept only in memory for this session. */
	const TArray<FMusubiNotification>& All() const { return Items; }
	int32 UnreadCount() const { return ServerUnread >= 0 ? ServerUnread : LocalUnread(); }
	const FMusubiNotification* Find(const FString& Id) const;

	FMusubiNotificationEvent OnReceived;
	FMusubiNotificationsChanged OnChanged;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Notifications")
	void RefreshInbox(FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Notifications")
	void MarkNotificationsRead(const TArray<FString>& Ids);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Notifications")
	void DeleteNotifications(const TArray<FString>& Ids);
	UFUNCTION(BlueprintPure, Category = "Musubi|Notifications")
	int32 GetUnreadCount() const { return UnreadCount(); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Notifications")
	TArray<FString> NotificationIds() const;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Notifications") FMusubiOnNotification OnNotification;

protected:
	virtual void OnResubscribe() override { Backfill(); }

private:
	TArray<FMusubiNotification> Items;
	TOptional<FString> Cursor;
	int32 ServerUnread = -1;
	int32 LocalUnread() const;
	void Merge(const FMusubiServerMessageNotification& N, bool bRead, bool bAnnounce);
	void Fetch(const FString& Path, bool bAppend, TFunction<void(bool)> OnDone);
	void Rest(const FString& Verb, const FString& Path, const TSharedPtr<FJsonObject>& Body, TFunction<void(bool, TSharedPtr<FJsonObject>)> OnDone);
};
