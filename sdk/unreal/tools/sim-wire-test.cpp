// Golden-vector test for the simulator stream types (MusubiServer), run against the UE shim:
//   for each vector {name, kind, json}: JSON text -> struct -> JSON equals the Rust JSON by value.
// Vectors come from `cargo test -p musubi-mesh --test sim_wire_vectors` (writes sim-vectors.json).
#include "MusubiSimCommandFrame.h"
#include "Serialization/JsonSerializer.h"
#include <fstream>
#include <sstream>
#include <iostream>

static bool Same(const TSharedPtr<FJsonValue>& A, const TSharedPtr<FJsonValue>& B)
{
	if (!A || !B) return !A && !B;
	if (A->IsNull() || B->IsNull()) return A->IsNull() && B->IsNull();
	if (A->Type != B->Type) return false;
	switch (A->Type)
	{
	case EJson::String: return A->Str == B->Str;
	case EJson::Number: return A->Num == B->Num;
	case EJson::Boolean: return A->B == B->B;
	case EJson::Array:
		if (A->Arr.Num() != B->Arr.Num()) return false;
		for (int32 I = 0; I < A->Arr.Num(); ++I) if (!Same(A->Arr[I], B->Arr[I])) return false;
		return true;
	case EJson::Object:
		if (A->Obj->Values.Num() != B->Obj->Values.Num()) return false;
		for (const auto& P : A->Obj->Values) { TSharedPtr<FJsonValue> O = B->Obj->TryGetField(P.Key); if (!O || !Same(P.Value, O)) return false; }
		return true;
	default: return true;
	}
}

template <typename T>
static int Check(const std::string& Name, const std::string& Json, int& Failed)
{
	auto Fail = [&](const char* What, const std::string& Got) { std::cout << "FAIL " << Name << ": " << What << "\n  got  " << Got << "\n  want " << Json << "\n"; ++Failed; return 1; };
	TSharedPtr<FJsonValue> Want = MusubiJson::Deserialize(FString(Json));
	T V;
	if (!T::FromJson(Want, V)) return Fail("json decode", Json);
	FString Back = MusubiJson::Serialize(T::ToJson(V));
	if (!Same(Want, MusubiJson::Deserialize(Back))) return Fail("json roundtrip", Back.S);
	return 0;
}

int main(int Argc, char** Argv)
{
	if (Argc < 2) { std::cerr << "usage: sim-wire-test sim-vectors.json\n"; return 2; }
	std::ifstream In(Argv[1]);
	std::stringstream Buf; Buf << In.rdbuf();
	TSharedPtr<FJsonValue> Root = MusubiJson::Deserialize(FString(Buf.str()));
	const TArray<TSharedPtr<FJsonValue>>* Arr;
	if (!Root || !Root->TryGetArray(Arr)) { std::cerr << "bad vectors file\n"; return 2; }
	int Failed = 0, Total = 0;
	for (const TSharedPtr<FJsonValue>& V : *Arr)
	{
		const TSharedPtr<FJsonObject>* O;
		if (!V->TryGetObject(O)) continue;
		FString Name, Kind, Json;
		(*O)->TryGetStringField("name", Name); (*O)->TryGetStringField("kind", Kind); (*O)->TryGetStringField("json", Json);
		++Total;
		if (Kind == "frame") Check<FMusubiSimFrame>(Name.S, Json.S, Failed);
		else if (Kind == "event") Check<FMusubiSimEvent>(Name.S, Json.S, Failed);
		else if (Kind == "command") Check<FMusubiSimCommandFrame>(Name.S, Json.S, Failed);
		else if (Kind == "CoLocateResult") Check<FMusubiCoLocateResult>(Name.S, Json.S, Failed);
		else if (Kind == "HandoverAckResult") Check<FMusubiHandoverAckResult>(Name.S, Json.S, Failed);
		else if (Kind == "RewindResult") Check<FMusubiRewindResult>(Name.S, Json.S, Failed);
		else if (Kind == "InteractResult") Check<FMusubiInteractResult>(Name.S, Json.S, Failed);
		else { std::cout << "FAIL " << Name.S << ": unknown kind " << Kind.S << "\n"; ++Failed; }
	}
	std::cout << (Total - Failed) << "/" << Total << " sim vectors ok\n";
	return Failed ? 1 : 0;
}
