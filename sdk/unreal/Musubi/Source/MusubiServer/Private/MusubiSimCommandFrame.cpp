#include "MusubiSimCommandFrame.h"

TSharedPtr<FJsonValue> FMusubiSimCommandFrame::ToJson(const FMusubiSimCommandFrame& V)
{
	TSharedPtr<FJsonValue> P = FMusubiSimCommand::ToJson(V.Cmd);
	if (V.Id.IsSet() && P.IsValid() && P->Type == EJson::Object)
		P->AsObject()->SetField(TEXT("id"), MusubiJson::JsonOrNull(V.Id.GetValue()));
	return P;
}

bool FMusubiSimCommandFrame::FromJson(const TSharedPtr<FJsonValue>& J, FMusubiSimCommandFrame& Out)
{
	const TSharedPtr<FJsonObject>* OP;
	if (!J.IsValid() || !J->TryGetObject(OP)) return false;
	Out.Id.Reset();
	if (TSharedPtr<FJsonValue> Id = (*OP)->TryGetField(TEXT("id")); Id.IsValid() && !Id->IsNull()) Out.Id = FMusubiJson(Id);
	return FMusubiSimCommand::FromJson(J, Out.Cmd);
}
