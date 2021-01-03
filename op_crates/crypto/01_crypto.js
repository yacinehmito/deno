// Copyright 2018-2020 the Deno authors. All rights reserved. MIT license.

// Implements https://www.w3.org/TR/WebCryptoAPI

((window) => {
  const core = window.Deno.core;

  function getRandomValues(typedArray) {
    if (typedArray == null) throw new Error("Input must not be null");
    if (typedArray.length > 65536) {
      throw new Error("Input must not be longer than 65536");
    }
    const ui8 = new Uint8Array(
      typedArray.buffer,
      typedArray.byteOffset,
      typedArray.byteLength,
    );
    core.jsonOpSync("op_get_random_values", {}, ui8);
    return typedArray;
  }

  const subtle = {
    async decrypt(algorithm, key, data) {
      throw new Error("Not implemented");
    },
    async deriveBits(algorithm, baseKey, length) {
      throw new Error("Not implemented");
    },
    async deriveKey(
      algorithm,
      baseKey,
      derivedKeyType,
      extractable,
      keyUsages,
    ) {
      throw new Error("Not implemented");
    },
    async digest(algorithm, data) {
      throw new Error("Not implemented");
    },
    async encrypt(algorithm, key, data) {
      throw new Error("Not implemented");
    },
    async exportKey(format, key) {
      throw new Error("Not implemented");
    },
    async generateKey(algorithm, extractable, keyUsages) {
      throw new Error("Not implemented");
    },
    async importKey(format, keyData, algorithm, extractable, keyUsages) {
      throw new Error("Not implemented");
    },
    async sign(algorithm, key, data) {
      throw new Error("Not implemented");
    },
    async unwrapKey(
      format,
      wrappedKey,
      unwrappingKey,
      unwrapAlgorithm,
      unwrappedKeyAlgorithm,
      extractable,
      keyUsages,
    ) {
      throw new Error("Not implemented");
    },
    async verify(algorithm, key, signature, data) {
      throw new Error("Not implemented");
    },
  };

  window.crypto = {
    getRandomValues,
    subtle,
  };
  window.__bootstrap = window.__bootstrap || {};
  window.__bootstrap.crypto = {
    getRandomValues,
    subtle,
  };
})(this);
