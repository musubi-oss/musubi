#pragma once

#include "CoreMinimal.h"
#include "Kismet/BlueprintAsyncActionBase.h"
#include "MusubiFacade.h"
#include "MusubiAsyncActions.generated.h"

DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiAsyncResult, const FMusubiCallResult&, Result);

/**
 * Latent Blueprint twins of the facade requests: one node per call with `On Done` / `On Failed` exec pins
 * (the `FMusubiCallResult` is the same the callback twins deliver: `Code`, `Message`, `PayloadJson`).
 * The action keeps itself alive via RegisterWithGameInstance until the reply, a timeout, or the facade is gone.
 */
UCLASS()
class MUSUBI_API UMusubiAsyncAction : public UBlueprintAsyncActionBase
{
	GENERATED_BODY()
public:
	UPROPERTY(BlueprintAssignable) FMusubiAsyncResult OnDone;
	UPROPERTY(BlueprintAssignable) FMusubiAsyncResult OnFailed;

	// Chat
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Room (Musubi)"))
	static UMusubiAsyncAction* ChatJoinRoom(UObject* WorldContextObject, const FString& Room, bool bPersistence = true, bool bHidden = false);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Direct Chat (Musubi)"))
	static UMusubiAsyncAction* ChatJoinDirect(UObject* WorldContextObject, const FString& AccountId, bool bPersistence = true);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Group Chat (Musubi)"))
	static UMusubiAsyncAction* ChatJoinGroup(UObject* WorldContextObject, const FString& GroupId, bool bPersistence = true, bool bHidden = false);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Send Chat Text (Musubi)"))
	static UMusubiAsyncAction* ChatSendText(UObject* WorldContextObject, const FString& ChannelId, const FString& Text);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Send Chat JSON (Musubi)"))
	static UMusubiAsyncAction* ChatSendJson(UObject* WorldContextObject, const FString& ChannelId, const FString& ContentJson, const FString& ReplyToMessageId);

	// Storage / notifications / presence
	UFUNCTION(BlueprintCallable, Category = "Musubi|Storage", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Subscribe Storage (Musubi)"))
	static UMusubiAsyncAction* StorageSubscribe(UObject* WorldContextObject, const FString& Collection, const FString& Key, const FString& OwnerId);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Notifications", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Refresh Inbox (Musubi)"))
	static UMusubiAsyncAction* NotificationsRefreshInbox(UObject* WorldContextObject);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Presence", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Follow Accounts (Musubi)"))
	static UMusubiAsyncAction* PresenceFollow(UObject* WorldContextObject, const TArray<FString>& AccountIds);

	// Matchmaker / parties / matches
	UFUNCTION(BlueprintCallable, Category = "Musubi|Matchmaker", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Matchmaker Enqueue (Musubi)", AutoCreateRefTerm = "StringProperties,NumericProperties"))
	static UMusubiAsyncAction* MatchmakerEnqueue(UObject* WorldContextObject, int32 MinCount, int32 MaxCount, const FString& Query, const TMap<FString, FString>& StringProperties, const TMap<FString, double>& NumericProperties, int32 CountMultiple = 1);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Parties", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Create Party (Musubi)"))
	static UMusubiAsyncAction* PartyCreate(UObject* WorldContextObject, bool bOpen = true, int32 MaxSize = 4, const FString& Label = TEXT(""), bool bHidden = false);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Parties", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Party (Musubi)"))
	static UMusubiAsyncAction* PartyJoin(UObject* WorldContextObject, const FString& PartyId);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Create Match (Musubi)"))
	static UMusubiAsyncAction* MatchCreate(UObject* WorldContextObject, const FString& Name, bool bSuppressPresence = false);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Match (Musubi)"))
	static UMusubiAsyncAction* MatchJoin(UObject* WorldContextObject, const FString& MatchId, bool bSpectator = false);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Match By Token (Musubi)"))
	static UMusubiAsyncAction* MatchJoinByToken(UObject* WorldContextObject, const FString& Token);

	// RPC
	UFUNCTION(BlueprintCallable, Category = "Musubi|RPC", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Call RPC (Musubi)"))
	static UMusubiAsyncAction* RpcCall(UObject* WorldContextObject, const FString& Id, const FString& PayloadJson);

	virtual void Activate() override;

private:
	TFunction<void(UMusubiAsyncAction*)> Start;
	bool bFinished = false;

	template <class TFacade>
	static UMusubiAsyncAction* Make(UObject* WorldContextObject, TFunction<void(TFacade*, FMusubiCallDone)> Call);
	static UMusubiAsyncAction* Fail(UObject* WorldContextObject, const FString& Code, const FString& Message);
	FMusubiCallDone Done();

	UFUNCTION() void HandleDone(const FMusubiCallResult& Result);
	UFUNCTION() void HandleRpc(bool bOk, const FString& PayloadJson, const FString& Error);
	void Finish(const FMusubiCallResult& Result);
};
