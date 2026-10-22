#include "MusubiMatchmaker.h"

void UMusubiMatchmaker::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::MatchmakerExpired, [this](const FMusubiServerMessage& M) {
		if (!M.MatchmakerExpired.IsSet()) return;
		if (FMusubiTicket* T = Active.Find(M.MatchmakerExpired->ticket))
		{
			SetState(*T, EMusubiTicketState::Expired);
			FMusubiTicket Copy = *T;
			Active.Remove(Copy.Ticket);
			OnExpired.Broadcast(Copy);
		}
	});
	Listen(EMusubiServerMessageTag::MatchmakerReadyCheck, [this](const FMusubiServerMessage& M) {
		if (!M.MatchmakerReadyCheck.IsSet()) return;
		FMusubiTicket& T = Active.FindOrAdd(M.MatchmakerReadyCheck->ticket);
		T.Ticket = M.MatchmakerReadyCheck->ticket;
		T.ReadyCheck = M.MatchmakerReadyCheck.GetValue();
		SetState(T, EMusubiTicketState::ReadyCheck);
		OnReadyCheck.Broadcast(T);
	});
	Listen(EMusubiServerMessageTag::MatchmakerReadyFailed, [this](const FMusubiServerMessage& M) {
		if (!M.MatchmakerReadyFailed.IsSet()) return;
		if (FMusubiTicket* T = Active.Find(M.MatchmakerReadyFailed->ticket))
		{
			T->ReadyCheck.Reset();
			// Requeued tickets go back to waiting; otherwise the ticket is dead (penalty applies on the node).
			SetState(*T, M.MatchmakerReadyFailed->requeued ? EMusubiTicketState::Queued : EMusubiTicketState::Failed);
			FMusubiTicket Copy = *T;
			if (!M.MatchmakerReadyFailed->requeued) Active.Remove(Copy.Ticket);
			OnReadyFailed.Broadcast(Copy);
		}
	});
	Listen(EMusubiServerMessageTag::MatchmakerMatched, [this](const FMusubiServerMessage& M) {
		if (!M.MatchmakerMatched.IsSet()) return;
		const auto& R = M.MatchmakerMatched.GetValue();
		FMusubiTicket& T = Active.FindOrAdd(R.ticket);
		T.Ticket = R.ticket;
		T.Matched = R;
		T.ReadyCheck.Reset();
		SetState(T, EMusubiTicketState::Matched);
		FMusubiTicket Copy = T;
		Active.Remove(R.ticket);
		OnMatched.Broadcast(Copy);
		OnMatchFound.Broadcast(R.ticket, R.token, R.server.IsSet() ? R.server->address : FString(), R.server.IsSet() ? (int32)R.server->port : 0,
			R.server.IsSet() ? R.server->connect_token : FString());
	});
	// Party tickets are announced to every member, not only the leader who added them.
	Listen(EMusubiServerMessageTag::PartyMatchmakerTicket, [this](const FMusubiServerMessage& M) {
		if (!M.PartyMatchmakerTicket.IsSet()) return;
		FMusubiTicket& T = Track(M.PartyMatchmakerTicket->ticket, M.PartyMatchmakerTicket->party_id, FMusubiClientMessageMatchmakerAdd());
		OnTicket.Broadcast(T);
	});
	Super::Initialize(Collection);
}

FMusubiTicket& UMusubiMatchmaker::Track(const FString& Ticket, const FString& PartyId, const FMusubiClientMessageMatchmakerAdd& Req)
{
	FMusubiTicket& T = Active.FindOrAdd(Ticket);
	if (T.Ticket.IsEmpty())
	{
		T.Ticket = Ticket;
		T.PartyId = PartyId;
		T.Request = Req;
		T.EnqueuedAt = FPlatformTime::Seconds();
		SetState(T, EMusubiTicketState::Queued);
	}
	return T;
}

void UMusubiMatchmaker::SetState(FMusubiTicket& T, EMusubiTicketState S)
{
	if (T.State == S) return;
	T.State = S;
	OnTicketChanged.Broadcast(T.Ticket, S);
}

FMusubiReply UMusubiMatchmaker::TicketReply(const FString& PartyId, FMusubiClientMessageMatchmakerAdd Req, FTicketDone OnDone)
{
	TWeakObjectPtr<UMusubiMatchmaker> Weak(this);
	return FMusubiReply::CreateLambda([Weak, PartyId, Req, OnDone](bool bOk, const FMusubiServerEnvelope& E) {
		FString Ticket;
		if (bOk && E.Msg.MatchmakerTicket.IsSet()) Ticket = E.Msg.MatchmakerTicket->ticket;
		else if (bOk && E.Msg.PartyMatchmakerTicket.IsSet()) Ticket = E.Msg.PartyMatchmakerTicket->ticket;
		if (!Ticket.IsEmpty())
		{
			const FMusubiTicket* T = nullptr;
			if (Weak.IsValid()) { FMusubiTicket& Tr = Weak->Track(Ticket, PartyId, Req); Weak->OnTicket.Broadcast(Tr); T = &Tr; }
			if (OnDone) OnDone(true, T, E);
		}
		else if (OnDone) OnDone(false, nullptr, bOk ? LocalError(EMusubiCodeTag::Internal, TEXT("unexpected reply")) : E);
	});
}

void UMusubiMatchmaker::Add(const FMusubiClientMessageMatchmakerAdd& Req, FTicketDone OnDone)
{
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchmakerAdd;
	M.MatchmakerAdd = Req;
	Request(MoveTemp(M), TicketReply(FString(), Req, OnDone));
}

void UMusubiMatchmaker::AddParty(const FString& PartyId, const FMusubiClientMessageMatchmakerAdd& Req, FTicketDone OnDone)
{
	FMusubiClientMessagePartyMatchmakerAdd P;
	P.party_id = PartyId;
	P.min_count = Req.min_count; P.max_count = Req.max_count; P.query = Req.query;
	P.string_properties = Req.string_properties; P.numeric_properties = Req.numeric_properties; P.count_multiple = Req.count_multiple;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::PartyMatchmakerAdd;
	M.PartyMatchmakerAdd = MoveTemp(P);
	Request(MoveTemp(M), TicketReply(PartyId, Req, OnDone));
}

void UMusubiMatchmaker::Remove(const FString& Ticket, FMusubiReply OnDone)
{
	FMusubiClientMessage M;
	FMusubiTicket Gone;
	if (Active.RemoveAndCopyValue(Ticket, Gone) && !Gone.PartyId.IsEmpty())
	{
		FMusubiClientMessagePartyMatchmakerRemove R;
		R.party_id = Gone.PartyId; R.ticket = Ticket;
		M.Tag = EMusubiClientMessageTag::PartyMatchmakerRemove;
		M.PartyMatchmakerRemove = MoveTemp(R);
	}
	else
	{
		FMusubiClientMessageMatchmakerRemove R;
		R.ticket = Ticket;
		M.Tag = EMusubiClientMessageTag::MatchmakerRemove;
		M.MatchmakerRemove = MoveTemp(R);
	}
	if (!Gone.Ticket.IsEmpty()) OnTicketChanged.Broadcast(Ticket, EMusubiTicketState::None);
	Request(MoveTemp(M), MoveTemp(OnDone));
}

void UMusubiMatchmaker::Ready(const FString& Ticket, FMusubiReply OnDone)
{
	FMusubiClientMessageMatchmakerReady R;
	R.ticket = Ticket;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::MatchmakerReady;
	M.MatchmakerReady = MoveTemp(R);
	Request(MoveTemp(M), MoveTemp(OnDone));
}

void UMusubiMatchmaker::RemoveAll()
{
	TArray<FString> Keys;
	Active.GetKeys(Keys);
	for (const FString& K : Keys) Remove(K);
}

void UMusubiMatchmaker::OnResubscribe()
{
	// Tickets belong to the session on the node; after a reconnect it either still holds them or
	// has expired them, and tells us via matchmaker_expired/matched. Solo tickets of a *new* session
	// are gone for good, so re-add from the stored request.
	TArray<FMusubiTicket> Solo;
	for (const auto& P : Active) if (P.Value.PartyId.IsEmpty() && P.Value.State == EMusubiTicketState::Queued) Solo.Add(P.Value);
	TSharedPtr<FMusubiSocket> S = Socket();
	const bool bFreshSession = S.IsValid() && S->GetSession().IsSet() && !S->GetSession()->resumed;
	if (bFreshSession)
	{
		for (const FMusubiTicket& T : Solo) { Active.Remove(T.Ticket); OnTicketChanged.Broadcast(T.Ticket, EMusubiTicketState::None); Add(T.Request); }
	}
}

// ---- Blueprint ----

void UMusubiMatchmaker::Enqueue(int32 MinCount, int32 MaxCount, const FString& Query, const TMap<FString, FString>& StringProperties,
	const TMap<FString, double>& NumericProperties, int32 CountMultiple, FMusubiCallDone OnDone)
{
	FMusubiClientMessageMatchmakerAdd R;
	R.min_count = (uint32)FMath::Max(1, MinCount);
	R.max_count = (uint32)FMath::Max(MinCount, MaxCount);
	R.query = Query.IsEmpty() ? TEXT("*") : Query;
	R.string_properties = StringProperties;
	R.numeric_properties = NumericProperties;
	R.count_multiple = (uint32)FMath::Max(1, CountMultiple);
	Add(R, [OnDone](bool bOk, const FMusubiTicket*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiMatchmaker::Dequeue(const FString& Ticket) { Remove(Ticket); }
void UMusubiMatchmaker::AcceptReadyCheck(const FString& Ticket) { Ready(Ticket); }

EMusubiTicketState UMusubiMatchmaker::TicketState(const FString& Ticket) const
{
	const FMusubiTicket* T = Active.Find(Ticket);
	return T ? T->State : EMusubiTicketState::None;
}
