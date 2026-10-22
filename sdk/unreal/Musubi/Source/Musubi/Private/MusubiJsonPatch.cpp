#include "MusubiJsonPatch.h"

namespace
{
	TArray<FString> Tokens(const FString& Pointer)
	{
		TArray<FString> Out;
		if (Pointer.IsEmpty()) return Out;
		FString P = Pointer;
		if (P[0] == '/') P.RemoveAt(0);
		P.ParseIntoArray(Out, TEXT("/"), false);
		for (FString& T : Out) { T.ReplaceInline(TEXT("~1"), TEXT("/")); T.ReplaceInline(TEXT("~0"), TEXT("~")); }
		return Out;
	}

	bool ArrayIndex(const FString& Tok, int32 Num, bool bAllowEnd, int32& Out)
	{
		if (bAllowEnd && Tok == TEXT("-")) { Out = Num; return true; }
		if (Tok.IsEmpty() || !Tok.IsNumeric()) return false;
		Out = FCString::Atoi(*Tok);
		return Out >= 0 && (bAllowEnd ? Out <= Num : Out < Num);
	}

	// Walk to the parent of the pointer's last token; returns the parent and the last token.
	TSharedPtr<FJsonValue> Parent(const TSharedPtr<FJsonValue>& Doc, const TArray<FString>& Toks, FString& Last)
	{
		TSharedPtr<FJsonValue> Cur = Doc;
		for (int32 i = 0; i + 1 < Toks.Num(); ++i)
		{
			if (!Cur.IsValid()) return nullptr;
			if (Cur->Type == EJson::Object) Cur = Cur->AsObject()->TryGetField(Toks[i]);
			else if (Cur->Type == EJson::Array)
			{
				const TArray<TSharedPtr<FJsonValue>>& A = Cur->AsArray();
				int32 Idx;
				if (!ArrayIndex(Toks[i], A.Num(), false, Idx)) return nullptr;
				Cur = A[Idx];
			}
			else return nullptr;
		}
		Last = Toks.Num() ? Toks.Last() : FString();
		return Cur;
	}

	bool Remove(TSharedPtr<FJsonValue>& Doc, const FString& Pointer, TSharedPtr<FJsonValue>* Removed, FString* Err)
	{
		TArray<FString> Toks = Tokens(Pointer);
		if (Toks.Num() == 0) { if (Removed) *Removed = Doc; Doc = MakeShared<FJsonValueNull>(); return true; }
		FString Last;
		TSharedPtr<FJsonValue> P = Parent(Doc, Toks, Last);
		if (!P.IsValid()) { if (Err) *Err = TEXT("remove: path not found ") + Pointer; return false; }
		if (P->Type == EJson::Object)
		{
			TSharedPtr<FJsonObject> O = P->AsObject();
			if (!O->HasField(Last)) { if (Err) *Err = TEXT("remove: missing key ") + Pointer; return false; }
			if (Removed) *Removed = O->TryGetField(Last);
			O->RemoveField(Last);
			return true;
		}
		if (P->Type == EJson::Array)
		{
			TArray<TSharedPtr<FJsonValue>>& A = const_cast<TArray<TSharedPtr<FJsonValue>>&>(P->AsArray());
			int32 Idx;
			if (!ArrayIndex(Last, A.Num(), false, Idx)) { if (Err) *Err = TEXT("remove: bad index ") + Pointer; return false; }
			if (Removed) *Removed = A[Idx];
			A.RemoveAt(Idx);
			return true;
		}
		if (Err) *Err = TEXT("remove: parent is a scalar ") + Pointer;
		return false;
	}

	bool Add(TSharedPtr<FJsonValue>& Doc, const FString& Pointer, const TSharedPtr<FJsonValue>& Value, bool bReplace, FString* Err)
	{
		TArray<FString> Toks = Tokens(Pointer);
		if (Toks.Num() == 0) { Doc = MusubiJsonPatch::Clone(Value); return true; }
		FString Last;
		TSharedPtr<FJsonValue> P = Parent(Doc, Toks, Last);
		if (!P.IsValid()) { if (Err) *Err = TEXT("add: path not found ") + Pointer; return false; }
		if (P->Type == EJson::Object)
		{
			TSharedPtr<FJsonObject> O = P->AsObject();
			if (bReplace && !O->HasField(Last)) { if (Err) *Err = TEXT("replace: missing key ") + Pointer; return false; }
			O->SetField(Last, MusubiJsonPatch::Clone(Value));
			return true;
		}
		if (P->Type == EJson::Array)
		{
			TArray<TSharedPtr<FJsonValue>>& A = const_cast<TArray<TSharedPtr<FJsonValue>>&>(P->AsArray());
			int32 Idx;
			if (!ArrayIndex(Last, A.Num(), !bReplace, Idx)) { if (Err) *Err = TEXT("add: bad index ") + Pointer; return false; }
			if (bReplace) A[Idx] = MusubiJsonPatch::Clone(Value);
			else A.Insert(MusubiJsonPatch::Clone(Value), Idx);
			return true;
		}
		if (Err) *Err = TEXT("add: parent is a scalar ") + Pointer;
		return false;
	}
}

namespace MusubiJsonPatch
{
	TSharedPtr<FJsonValue> Clone(const TSharedPtr<FJsonValue>& V)
	{
		if (!V.IsValid()) return nullptr;
		switch (V->Type)
		{
		case EJson::Object:
		{
			TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
			for (const auto& KV : V->AsObject()->Values) O->SetField(KV.Key, Clone(KV.Value));
			return MakeShared<FJsonValueObject>(O);
		}
		case EJson::Array:
		{
			TArray<TSharedPtr<FJsonValue>> A;
			for (const TSharedPtr<FJsonValue>& E : V->AsArray()) A.Add(Clone(E));
			return MakeShared<FJsonValueArray>(A);
		}
		case EJson::String: return MakeShared<FJsonValueString>(V->AsString());
		case EJson::Number: return MakeShared<FJsonValueNumber>(V->AsNumber());
		case EJson::Boolean: return MakeShared<FJsonValueBoolean>(V->AsBool());
		default: return MakeShared<FJsonValueNull>();
		}
	}

	bool Equal(const TSharedPtr<FJsonValue>& A, const TSharedPtr<FJsonValue>& B)
	{
		if (!A.IsValid() || !B.IsValid()) return A.IsValid() == B.IsValid();
		if (A->IsNull() || B->IsNull()) return A->IsNull() && B->IsNull();
		if (A->Type != B->Type) return false;
		switch (A->Type)
		{
		case EJson::String: return A->AsString() == B->AsString();
		case EJson::Number: return A->AsNumber() == B->AsNumber();
		case EJson::Boolean: return A->AsBool() == B->AsBool();
		case EJson::Array:
		{
			const TArray<TSharedPtr<FJsonValue>>& X = A->AsArray(); const TArray<TSharedPtr<FJsonValue>>& Y = B->AsArray();
			if (X.Num() != Y.Num()) return false;
			for (int32 i = 0; i < X.Num(); ++i) if (!Equal(X[i], Y[i])) return false;
			return true;
		}
		case EJson::Object:
		{
			const TSharedPtr<FJsonObject>& X = A->AsObject(); const TSharedPtr<FJsonObject>& Y = B->AsObject();
			if (X->Values.Num() != Y->Values.Num()) return false;
			for (const auto& KV : X->Values) { TSharedPtr<FJsonValue> O = Y->TryGetField(KV.Key); if (!O.IsValid() || !Equal(KV.Value, O)) return false; }
			return true;
		}
		default: return true;
		}
	}

	TSharedPtr<FJsonValue> Get(const TSharedPtr<FJsonValue>& Doc, const FString& Pointer)
	{
		TArray<FString> Toks = Tokens(Pointer);
		TSharedPtr<FJsonValue> Cur = Doc;
		for (const FString& T : Toks)
		{
			if (!Cur.IsValid()) return nullptr;
			if (Cur->Type == EJson::Object) Cur = Cur->AsObject()->TryGetField(T);
			else if (Cur->Type == EJson::Array) { int32 Idx; if (!ArrayIndex(T, Cur->AsArray().Num(), false, Idx)) return nullptr; Cur = Cur->AsArray()[Idx]; }
			else return nullptr;
		}
		return Cur;
	}

	bool Apply(TSharedPtr<FJsonValue>& Doc, const TArray<TSharedPtr<FJsonValue>>& Ops, FString* Error)
	{
		if (!Doc.IsValid()) Doc = MakeShared<FJsonValueNull>();
		for (const TSharedPtr<FJsonValue>& OpV : Ops)
		{
			const TSharedPtr<FJsonObject>* OpO;
			if (!OpV.IsValid() || !OpV->TryGetObject(OpO)) { if (Error) *Error = TEXT("op is not an object"); return false; }
			FString Op, Path, From;
			(*OpO)->TryGetStringField(TEXT("op"), Op);
			(*OpO)->TryGetStringField(TEXT("path"), Path);
			(*OpO)->TryGetStringField(TEXT("from"), From);
			TSharedPtr<FJsonValue> Value = (*OpO)->TryGetField(TEXT("value"));
			if (Op == TEXT("add")) { if (!Add(Doc, Path, Value, false, Error)) return false; }
			else if (Op == TEXT("replace")) { if (!Add(Doc, Path, Value, true, Error)) return false; }
			else if (Op == TEXT("remove")) { if (!Remove(Doc, Path, nullptr, Error)) return false; }
			else if (Op == TEXT("move"))
			{
				TSharedPtr<FJsonValue> Moved;
				if (!Remove(Doc, From, &Moved, Error)) return false;
				if (!Add(Doc, Path, Moved, false, Error)) return false;
			}
			else if (Op == TEXT("copy"))
			{
				TSharedPtr<FJsonValue> Src = Get(Doc, From);
				if (!Src.IsValid()) { if (Error) *Error = TEXT("copy: from not found ") + From; return false; }
				if (!Add(Doc, Path, Src, false, Error)) return false;
			}
			else if (Op == TEXT("test"))
			{
				if (!Equal(Get(Doc, Path), Value)) { if (Error) *Error = TEXT("test failed at ") + Path; return false; }
			}
			else { if (Error) *Error = TEXT("unknown op ") + Op; return false; }
		}
		return true;
	}
}
