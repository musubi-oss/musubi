// Matchmaker facade: solo and party tickets, expiry, matched result (peer presences or an
// allocated game server), ready check (wire tags matchmaker_* and party_matchmaker_*).
#pragma once

#include "MusubiFacade.h"
#include "MusubiMatchmaker.generated.h"

UENUM(BlueprintType)
enum class EMusubiTicketState : uint8
{
	None,
	Queued,
	ReadyCheck,
	Matched,
	Expired,
	Failed,
};

struct MUSUBI_API FMusubiTicket
{
	FString Ticket;
	/** Set for party tickets. */
	FString PartyId;
	EMusubiTicketState State = EMusubiTicketState::None;
	FMusubiClientMessageMatchmakerAdd Request;
	TOptional<FMusubiServerMessageMatchmakerReadyCheck> ReadyCheck;
	TOptional<FMusubiServerMessageMatchmakerMatched> Matched;
	double EnqueuedAt = 0;
};

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiTicketEvent, const FMusubiTicket&);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnTicketChanged, const FString&, Ticket, EMusubiTicketState, State);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FiveParams(FMusubiOnMatched, const FString&, Ticket, const FString&, Token, const FString&, ServerAddress, int32, ServerPort, const FString&, ConnectToken);

UCLASS()
class MUSUBI_API UMusubiMatchmaker : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	typedef TFunction<void(bool, const FMusubiTicket*, const FMusubiServerEnvelope&)> FTicketDone;
	/** Solo ticket. `Query` is the node's matchmaker query language (`*` = anyone). */
	void Add(const FMusubiClientMessageMatchmakerAdd& Req, FTicketDone OnDone = nullptr);
	/** Party ticket (leader only on the node). */
	void AddParty(const FString& PartyId, const FMusubiClientMessageMatchmakerAdd& Req, FTicketDone OnDone = nullptr);
	void Remove(const FString& Ticket, FMusubiReply OnDone = FMusubiReply());
	/** Answer a ready check. */
	void Ready(const FString& Ticket, FMusubiReply OnDone = FMusubiReply());
	void RemoveAll();

	const TMap<FString, FMusubiTicket>& Tickets() const { return Active; }
	const FMusubiTicket* Find(const FString& Ticket) const { return Active.Find(Ticket); }

	FMusubiTicketEvent OnTicket;
	FMusubiTicketEvent OnExpired;
	FMusubiTicketEvent OnReadyCheck;
	FMusubiTicketEvent OnReadyFailed;
	FMusubiTicketEvent OnMatched;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Matchmaker", meta = (AutoCreateRefTerm = "StringProperties,NumericProperties"))
	void Enqueue(int32 MinCount, int32 MaxCount, const FString& Query, const TMap<FString, FString>& StringProperties, const TMap<FString, double>& NumericProperties, int32 CountMultiple, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Matchmaker")
	void Dequeue(const FString& Ticket);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Matchmaker")
	void AcceptReadyCheck(const FString& Ticket);
	UFUNCTION(BlueprintPure, Category = "Musubi|Matchmaker")
	EMusubiTicketState TicketState(const FString& Ticket) const;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Matchmaker") FMusubiOnTicketChanged OnTicketChanged;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Matchmaker") FMusubiOnMatched OnMatchFound;

protected:
	virtual void OnResubscribe() override;

private:
	TMap<FString, FMusubiTicket> Active;
	FMusubiTicket& Track(const FString& Ticket, const FString& PartyId, const FMusubiClientMessageMatchmakerAdd& Req);
	void SetState(FMusubiTicket& T, EMusubiTicketState S);
	FMusubiReply TicketReply(const FString& PartyId, FMusubiClientMessageMatchmakerAdd Req, FTicketDone OnDone);
};
