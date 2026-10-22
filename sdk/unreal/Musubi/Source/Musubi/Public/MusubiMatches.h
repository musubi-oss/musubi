// Match facade: relayed/authoritative matches this session is in — join/leave, data frames,
// host transfer, presence (wire tags match_*). Tick/state/input (rollback) land in a later phase.
#pragma once

#include "MusubiFacade.h"
#include "MusubiMatches.generated.h"

struct MUSUBI_API FMusubiMatchState
{
	FMusubiServerMessageMatch Info;
	/** Join arguments kept for re-join after a reconnect. */
	FMusubiClientMessageMatchJoin JoinArgs;
	uint64 FramesIn = 0;
	uint64 FramesOut = 0;
	bool IsHost() const { return Info.host.IsSet() && Info.host->session_id == Info.me.session_id; }
};

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiMatchEvent, const FMusubiMatchState&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiMatchDataEvent, const FMusubiMatchState&, const FMusubiServerMessageMatchData&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiMatchPresenceDelegate, const FMusubiMatchState&, const FMusubiServerMessageMatchPresenceEvent&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiMatchHostDelegate, const FMusubiMatchState&, const FMusubiUserPresence&);

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnMatchChanged, const FString&, MatchId, bool, bJoined);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FMusubiOnMatchData, const FString&, MatchId, const FString&, SenderUsername, int64, OpCode, const TArray<uint8>&, Data);

UCLASS()
class MUSUBI_API UMusubiMatches : public UMusubiFacade
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	typedef TFunction<void(bool, const FMusubiMatchState*, const FMusubiServerEnvelope&)> FMatchDone;
	void Create(TOptional<FString> Name, bool bSuppressPresence, FMatchDone OnDone = nullptr);
	/** Join by id or by matchmaker token (set one of them); `Spectator` joins read-only, `DelayTicks` lags the feed. */
	void Join(const FMusubiClientMessageMatchJoin& Args, FMatchDone OnDone = nullptr);
	void Leave(const FString& MatchId);
	/** Relay a frame; empty `Presences` = everyone else. Unreliable frames may be dropped under load. */
	void SendData(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable = true, const TArray<FMusubiUserPresence>& Presences = {},
		TOptional<FMusubiChannel> Channel = {});
	void TransferHost(const FString& MatchId, const FMusubiUuid& SessionId, FMusubiReply OnDone = FMusubiReply());

	const FMusubiMatchState* Match(const FString& MatchId) const { return Active.Find(MatchId); }
	const TMap<FString, FMusubiMatchState>& Matches() const { return Active; }

	FMusubiMatchEvent OnJoined;
	FMusubiMatchEvent OnLeft;
	FMusubiMatchDataEvent OnData;
	FMusubiMatchPresenceDelegate OnPresence;
	FMusubiMatchHostDelegate OnHost;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match")
	void CreateMatch(const FString& Name, bool bSuppressPresence, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match")
	void JoinMatch(const FString& MatchId, bool bSpectator, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match")
	void JoinMatchByToken(const FString& Token, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match")
	void LeaveMatch(const FString& MatchId);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Match")
	void SendMatchData(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable = true);
	UFUNCTION(BlueprintPure, Category = "Musubi|Match")
	bool IsMatchHost(const FString& MatchId) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Match")
	TArray<FString> MatchMemberNames(const FString& MatchId) const;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Match") FMusubiOnMatchChanged OnMatchChanged;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Match") FMusubiOnMatchData OnMatchData;

protected:
	virtual void OnResubscribe() override;

private:
	TMap<FString, FMusubiMatchState> Active;
	FMusubiReply MatchReply(FMusubiClientMessageMatchJoin Args, FMatchDone OnDone);
};
