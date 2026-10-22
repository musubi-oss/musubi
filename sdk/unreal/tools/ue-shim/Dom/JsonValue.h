#pragma once
#include "CoreMinimal.h"
class FJsonObject;
enum class EJson { None, Null, String, Number, Boolean, Array, Object };
class FJsonValue
{
public:
	EJson Type = EJson::None;
	FString Str; double Num = 0; bool B = false;
	TArray<TSharedPtr<FJsonValue>> Arr;
	TSharedPtr<FJsonObject> Obj;
	virtual ~FJsonValue() = default;
	bool IsNull() const { return Type == EJson::Null || Type == EJson::None; }
	bool TryGetString(FString& Out) const { if (Type != EJson::String) return false; Out = Str; return true; }
	bool TryGetNumber(double& Out) const { if (Type != EJson::Number) return false; Out = Num; return true; }
	bool TryGetNumber(int32& Out) const { if (Type != EJson::Number) return false; Out = (int32)Num; return true; }
	bool TryGetNumber(uint32& Out) const { if (Type != EJson::Number) return false; Out = (uint32)Num; return true; }
	bool TryGetNumber(int64& Out) const { if (Type != EJson::Number) return false; Out = (int64)Num; return true; }
	bool TryGetNumber(uint64& Out) const { if (Type != EJson::Number) return false; Out = (uint64)Num; return true; }
	bool TryGetNumber(uint16& Out) const { if (Type != EJson::Number) return false; Out = (uint16)Num; return true; }
	bool TryGetBool(bool& Out) const { if (Type != EJson::Boolean) return false; Out = B; return true; }
	bool TryGetArray(const TArray<TSharedPtr<FJsonValue>>*& Out) const { if (Type != EJson::Array) return false; Out = &Arr; return true; }
	bool TryGetObject(const TSharedPtr<FJsonObject>*& Out) const { if (Type != EJson::Object) return false; Out = &Obj; return true; }
	const TSharedPtr<FJsonObject>& AsObject() const { return Obj; }
	FString AsString() const { return Str; }
	double AsNumber() const { return Num; }
};
struct FJsonValueNull : FJsonValue { FJsonValueNull() { Type = EJson::Null; } };
struct FJsonValueString : FJsonValue { explicit FJsonValueString(const FString& S) { Type = EJson::String; Str = S; } };
struct FJsonValueNumber : FJsonValue { explicit FJsonValueNumber(double D) { Type = EJson::Number; Num = D; } };
struct FJsonValueBoolean : FJsonValue { explicit FJsonValueBoolean(bool V) { Type = EJson::Boolean; B = V; } };
struct FJsonValueArray : FJsonValue { explicit FJsonValueArray(const TArray<TSharedPtr<FJsonValue>>& A) { Type = EJson::Array; Arr = A; } };
struct FJsonValueObject : FJsonValue { explicit FJsonValueObject(const TSharedPtr<FJsonObject>& O) { Type = EJson::Object; Obj = O; } };
