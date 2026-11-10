"""Windows Credential Manager adapter for personal Gitea tokens."""

import ctypes
from ctypes import wintypes

CRED_TYPE_GENERIC = 1

CRED_PERSIST_LOCAL_MACHINE = 2

ERROR_NOT_FOUND = 1168

class Credential(ctypes.Structure):
    _fields_ = [
        ("Flags", wintypes.DWORD),
        ("Type", wintypes.DWORD),
        ("TargetName", wintypes.LPWSTR),
        ("Comment", wintypes.LPWSTR),
        ("LastWritten", wintypes.FILETIME),
        ("CredentialBlobSize", wintypes.DWORD),
        ("CredentialBlob", ctypes.c_void_p),
        ("Persist", wintypes.DWORD),
        ("AttributeCount", wintypes.DWORD),
        ("Attributes", ctypes.c_void_p),
        ("TargetAlias", wintypes.LPWSTR),
        ("UserName", wintypes.LPWSTR),
    ]

class WindowsTokenStore:
    def __init__(self, target):
        self.target = target
        self.api = ctypes.WinDLL("advapi32", use_last_error=True)
        credential_pointer = ctypes.POINTER(Credential)
        self.api.CredWriteW.argtypes = (credential_pointer, wintypes.DWORD)
        self.api.CredWriteW.restype = wintypes.BOOL
        self.api.CredReadW.argtypes = (
            wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(credential_pointer)
        )
        self.api.CredReadW.restype = wintypes.BOOL
        self.api.CredDeleteW.argtypes = (wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD)
        self.api.CredDeleteW.restype = wintypes.BOOL
        self.api.CredFree.argtypes = (ctypes.c_void_p,)
        self.api.CredFree.restype = None

    def load(self):
        entry = ctypes.POINTER(Credential)()
        if not self.api.CredReadW(self.target, CRED_TYPE_GENERIC, 0, ctypes.byref(entry)):
            error = ctypes.get_last_error()
            if error == ERROR_NOT_FOUND:
                return None
            raise ctypes.WinError(error)
        try:
            data = entry.contents
            return ctypes.string_at(data.CredentialBlob, data.CredentialBlobSize).decode("utf-16-le")
        finally:
            self.api.CredFree(entry)

    def save(self, token):
        encoded = token.encode("utf-16-le")
        buffer = ctypes.create_string_buffer(encoded, len(encoded))
        entry = Credential(
            Type=CRED_TYPE_GENERIC,
            TargetName=self.target,
            CredentialBlobSize=len(encoded),
            CredentialBlob=ctypes.addressof(buffer),
            Persist=CRED_PERSIST_LOCAL_MACHINE,
            UserName="CF Wizard Gitea Pilot",
        )
        try:
            if not self.api.CredWriteW(ctypes.byref(entry), 0):
                raise ctypes.WinError(ctypes.get_last_error())
        finally:
            ctypes.memset(buffer, 0, len(encoded))

    def delete(self):
        if self.api.CredDeleteW(self.target, CRED_TYPE_GENERIC, 0):
            return True
        error = ctypes.get_last_error()
        if error == ERROR_NOT_FOUND:
            return False
        raise ctypes.WinError(error)
