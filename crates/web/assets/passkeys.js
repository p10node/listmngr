// Passkeys need the browser's WebAuthn API, which is script-only. Every form
// on these pages still works without this file; it only adds the ceremonies.
(() => {
  'use strict';
  const supported =
    'PublicKeyCredential' in window &&
    typeof PublicKeyCredential.parseCreationOptionsFromJSON === 'function' &&
    typeof PublicKeyCredential.parseRequestOptionsFromJSON === 'function';
  const csrf = () => {
    const field = document.querySelector('input[name="csrf"]');
    return field ? field.value : '';
  };
  const post = (url, body) =>
    fetch(url, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': csrf() },
      body: JSON.stringify(body),
    });
  const report = (element, key) => {
    if (element) element.textContent = element.dataset[key] || '';
  };

  const register = document.getElementById('passkey-register');
  if (register) {
    const status = document.getElementById('passkey-status');
    if (!supported) {
      register.hidden = true;
      report(status, 'unsupported');
    } else {
      register.addEventListener('submit', async (event) => {
        event.preventDefault();
        report(status, 'working');
        try {
          const start = await post('/web/account/passkeys/register/start', {});
          if (!start.ok) throw new Error('start');
          const options = PublicKeyCredential.parseCreationOptionsFromJSON(await start.json());
          const credential = await navigator.credentials.create({ publicKey: options });
          const finish = await post('/web/account/passkeys/register/finish', {
            name: register.elements.name.value,
            credential: credential.toJSON(),
          });
          if (!finish.ok) throw new Error('finish');
          window.location.reload();
        } catch (error) {
          report(status, 'failed');
        }
      });
    }
  }

  const login = document.getElementById('passkey-login');
  if (login) {
    const status = document.getElementById('passkey-login-status');
    // The button is hidden in the markup: without this script, or without
    // WebAuthn, there is nothing it could do.
    login.hidden = !supported;
    if (supported) {
      login.addEventListener('click', async () => {
        report(status, 'working');
        try {
          const start = await post('/web/login/passkey/start', {});
          if (!start.ok) throw new Error('start');
          const options = PublicKeyCredential.parseRequestOptionsFromJSON(await start.json());
          const assertion = await navigator.credentials.get({ publicKey: options });
          const finish = await post('/web/login/passkey/finish', assertion.toJSON());
          if (!finish.ok) throw new Error('finish');
          const reply = await finish.json();
          window.location.assign(reply.next || '/web/account');
        } catch (error) {
          report(status, 'failed');
        }
      });
    }
  }
})();
