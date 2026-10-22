// Party facade: the one party this session is in (membership, leader, join requests, data
// frames), public party listing, invites; re-joins the party after a reconnect (wire tags party_*).
#pragma once

#include "MusubiFacade.h"
#include "MusubiParties.generated.h"

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyEvent, const FMusubiServerMessageParty&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyPresenceDelegate, const FMusubiServerMessagePartyPresenceEvent&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyJoinRequestDelegate, const FMusubiServerMessagePartyJoinRequest&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyLeaderDelegate, const FMusubiServerMessagePartyLeader&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyDataDelegate, const FMusubiServerMessagePartyData&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyInviteDelegate, const FMusubiServerMessagePartyInvite&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiPartyClosedDelegate, const FString& /*PartyId*/);

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnPartyChanged, const FString&, PartyId, bool, bInParty);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnPartyData, const FString&, PartyId, int64, OpCode, const TArray<uint8>&, Data);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnPartyInvite, const FString&, PartyId, const FString&, FromAccountId, const FString&, FromUsername);

UCLASS()
class MUSUBI_API UMusubiParties : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	typedef TFunction<void(bool, const FMusubiServerMessageParty*, const FMusubiServerEnvelope&)> FPartyDone;
	void Create(bool bOpen, uint32 MaxSize, TOptional<FString> Label, bool bHidden, FPartyDone OnDone = nullptr);
	void Join(const FString& PartyId, FPartyDone OnDone = nullptr);
	void Leave();
	void Close();
	void Update(TOptional<FString> Label, TOptional<bool> bOpen, TOptional<bool> bHidden, TOptional<uint32> MaxSize, FMusubiReply OnDone = FMusubiReply());
	void Promote(const FMusubiUserPresence& Member, FMusubiReply OnDone = FMusubiReply());
	void Accept(const FMusubiUserPresence& Requester, FMusubiReply OnDone = FMusubiReply());
	void Remove(const FMusubiUserPresence& Member, FMusubiReply OnDone = FMusubiReply());
	void Invite(const FMusubiUuid& AccountId, FMusubiReply OnDone = FMusubiReply());
	void ListJoinRequests(TFunction<void(bool, const TArray<FMusubiUserPresence>&)> OnDone);
	void SendData(int64 OpCode, const TArray<uint8>& Data);
	void List(TOptional<FString> Label, TOptional<FString> LabelPrefix, TOptional<bool> bOpen, uint32 Limit,
		TFunction<void(bool, const TArray<FMusubiPartyListing>&)> OnDone);

	bool InParty() const { return Current.IsSet(); }
	const FMusubiServerMessageParty* Party() const { return Current.GetPtrOrNull(); }
	bool IsLeader() const { return Current.IsSet() && Current->leader.session_id == Current->me.session_id; }
	const TArray<FMusubiUserPresence>& PendingJoinRequests() const { return JoinRequests; }

	FMusubiPartyEvent OnJoined;
	FMusubiPartyEvent OnUpdated;
	FMusubiPartyClosedDelegate OnLeft;
	FMusubiPartyPresenceDelegate OnPresence;
	FMusubiPartyJoinRequestDelegate OnJoinRequest;
	FMusubiPartyLeaderDelegate OnLeader;
	FMusubiPartyDataDelegate OnData;
	FMusubiPartyInviteDelegate OnInvite;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Party")
	void CreateParty(bool bOpen, int32 MaxSize, const FString& Label, bool bHidden, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Party")
	void JoinParty(const FString& PartyId, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Party")
	void LeaveParty();
	UFUNCTION(BlueprintCallable, Category = "Musubi|Party")
	void SendPartyData(int64 OpCode, const TArray<uint8>& Data);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Party")
	void InviteToParty(const FString& AccountId);
	UFUNCTION(BlueprintPure, Category = "Musubi|Party")
	FString CurrentPartyId() const { return Current.IsSet() ? Current->party_id : FString(); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Party")
	bool IsPartyLeader() const { return IsLeader(); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Party")
	TArray<FString> PartyMemberNames() const;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Party") FMusubiOnPartyChanged OnPartyChanged;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Party") FMusubiOnPartyData OnPartyData;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Party") FMusubiOnPartyInvite OnPartyInvite;

protected:
	virtual void OnResubscribe() override;
	virtual void OnDisconnected() override;

private:
	TOptional<FMusubiServerMessageParty> Current;
	TArray<FMusubiUserPresence> JoinRequests;
	/** Party id to re-join after a reconnect (membership is per session on the node). */
	FString RejoinId;
	void SetParty(const FMusubiServerMessageParty& P, bool bFresh);
	void ClearParty(const FString& Why);
	FMusubiReply PartyReply(FPartyDone OnDone);
	template <typename TMsg> FMusubiClientMessage WithParty(EMusubiClientMessageTag Tag, TOptional<TMsg> FMusubiClientMessage::*Field, TMsg&& V)
	{
		FMusubiClientMessage M;
		M.Tag = Tag;
		M.*Field = MoveTemp(V);
		return M;
	}
};
