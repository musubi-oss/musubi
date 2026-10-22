#include "MusubiMatches.h"

void UMusubiMatches::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::MatchData, [this](const FMusubiServerMessage& M) {
		if (!M.MatchData.IsSet()) return;
		const auto& D = M.MatchData.GetValue();
		FMusubiMatchState* S = Active.Find(D.match_id);
		if (!S) return;
		S->FramesIn++;
		OnData.Broadcast(*S, D);
		OnMatchData.Broadcast(D.match_id, D.presence.IsSet() ? D.presence->username : FString(), D.op_code, D.data);
	});
	Listen(EMusubiServerMessageTag::MatchPresenceEvent, [this](const FMusubiServerMessage& M) {
		if (!M.MatchPresenceEvent.IsSet()) return;
		const auto& E = M.MatchPresenceEvent.GetValue();
		FMusubiMatchState* S = Active.Find(E.match_id);
		if (!S) return;
		for (const FMusubiUserPresence& P : E.leaves)
			S->Info.presences.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
		for (const FMusubiUserPresence& P : E.joins)
		{
			S->Info.presences.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
			S->Info.presences.Add(P);
		}
		S->Info.size = (uint32)S->Info.presences.Num();
		OnPresence.Broadcast(*S, E);
	});
	Listen(EMusubiServerMessageTag::MatchHost, [this](const FMusubiServerMessage& M) {
		if (!M.MatchHost.IsSet()) return;
		if (FMusubiMatchState* S = Active.Find(M.MatchHost->match_id))
		{
			S->Info.host = M.MatchHost->host;
			OnHost.Broadcast(*S, M.MatchHost->host);
		}
	});
	Super::Initialize(Collection);
}

FMusubiReply UMusubiMatches::MatchReply(FMusubiClientMessageMatchJoin Args, FMatchDone OnDone)
{
	TWeakObjectPtr<UMusubiMatches> Weak(this);
	return FMusubiReply::CreateLambda([Weak, Args, OnDone](bool bOk, const FMusubiServerEnvelope& E) {
		if (bOk && E.Msg.Match.IsSet())
		{
			const FMusubiMatchState* Out = nullptr;
			if (Weak.IsValid())
			{
				const auto& Info = E.Msg.Match.GetValue();
				FMusubiMatchState& S = Weak->Active.FindOrAdd(Info.match_id);
				S.Info = Info;
				S.JoinArgs = Args;
				S.JoinArgs.match_id = Info.match_id;
				S.JoinArgs.token.Reset();
				Weak->OnJoined.Broadcast(S);
				Weak->OnMatchChanged.Broadcast(Info.match_id, true);
				Out = &S;
			}
			if (OnDone) OnDone(true, Out, E);
		}
		else if (OnDone) OnDone(false, nullptr, bOk ? LocalError(EMusubiCodeTag::Internal, TEXT("unexpected reply")) : E);
	});
}

void UMusubiMatches::Create(TOptional<FString> Name, bool bSuppressPresence, FMatchDone OnDone)
{
	FMusubiClientMessageMatchCreate C;
	C.name = Name; C.suppress_presence = bSuppressPresence;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchCreate;
	M.MatchCreate = MoveTemp(C);
	FMusubiClientMessageMatchJoin Rejoin;
	Request(MoveTemp(M), MatchReply(Rejoin, OnDone));
}

void UMusubiMatches::Join(const FMusubiClientMessageMatchJoin& Args, FMatchDone OnDone)
{
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchJoin;
	M.MatchJoin = Args;
	Request(MoveTemp(M), MatchReply(Args, OnDone));
}

void UMusubiMatches::Leave(const FString& MatchId)
{
	FMusubiClientMessageMatchLeave L;
	L.match_id = MatchId;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchLeave;
	M.MatchLeave = MoveTemp(L);
	Send(MoveTemp(M));
	FMusubiMatchState Gone;
	if (Active.RemoveAndCopyValue(MatchId, Gone))
	{
		OnLeft.Broadcast(Gone);
		OnMatchChanged.Broadcast(MatchId, false);
	}
}

void UMusubiMatches::SendData(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable, const TArray<FMusubiUserPresence>& Presences,
	TOptional<FMusubiChannel> Channel)
{
	FMusubiClientMessageMatchDataSend D;
	D.match_id = MatchId; D.op_code = OpCode; D.data = Data; D.presences = Presences; D.reliable = bReliable; D.channel = Channel;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchDataSend;
	M.MatchDataSend = MoveTemp(D);
	if (Send(MoveTemp(M)))
		if (FMusubiMatchState* S = Active.Find(MatchId)) S->FramesOut++;
}

void UMusubiMatches::TransferHost(const FString& MatchId, const FMusubiUuid& SessionId, FMusubiReply OnDone)
{
	FMusubiClientMessageMatchHostTransfer T;
	T.match_id = MatchId; T.session_id = SessionId;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchHostTransfer;
	M.MatchHostTransfer = MoveTemp(T);
	Request(MoveTemp(M), MoveTemp(OnDone));
}

void UMusubiMatches::OnResubscribe()
{
	TArray<FMusubiClientMessageMatchJoin> Again;
	for (const auto& P : Active) Again.Add(P.Value.JoinArgs);
	TWeakObjectPtr<UMusubiMatches> Weak(this);
	for (const FMusubiClientMessageMatchJoin& J : Again)
	{
		const FString Id = J.match_id.Get(FString());
		Join(J, [Weak, Id](bool bOk, const FMusubiMatchState*, const FMusubiServerEnvelope&) {
			FMusubiMatchState Gone;
			if (!bOk && Weak.IsValid() && Weak->Active.RemoveAndCopyValue(Id, Gone))
			{
				Weak->OnLeft.Broadcast(Gone);
				Weak->OnMatchChanged.Broadcast(Id, false);
			}
		});
	}
}

// ---- Blueprint ----

void UMusubiMatches::CreateMatch(const FString& Name, bool bSuppressPresence, FMusubiCallDone OnDone)
{
	Create(Name.IsEmpty() ? TOptional<FString>() : TOptional<FString>(Name), bSuppressPresence,
		[OnDone](bool bOk, const FMusubiMatchState*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiMatches::JoinMatch(const FString& MatchId, bool bSpectator, FMusubiCallDone OnDone)
{
	FMusubiClientMessageMatchJoin J;
	J.match_id = MatchId; J.spectator = bSpectator;
	Join(J, [OnDone](bool bOk, const FMusubiMatchState*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiMatches::JoinMatchByToken(const FString& Token, FMusubiCallDone OnDone)
{
	FMusubiClientMessageMatchJoin J;
	J.token = Token;
	Join(J, [OnDone](bool bOk, const FMusubiMatchState*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiMatches::LeaveMatch(const FString& MatchId) { Leave(MatchId); }
void UMusubiMatches::SendMatchData(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable) { SendData(MatchId, OpCode, Data, bReliable); }

bool UMusubiMatches::IsMatchHost(const FString& MatchId) const
{
	const FMusubiMatchState* S = Active.Find(MatchId);
	return S && S->IsHost();
}

TArray<FString> UMusubiMatches::MatchMemberNames(const FString& MatchId) const
{
	TArray<FString> Out;
	if (const FMusubiMatchState* S = Active.Find(MatchId)) for (const FMusubiUserPresence& P : S->Info.presences) Out.Add(P.username);
	return Out;
}
