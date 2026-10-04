/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

/**
 * Minimal RU/EN localization for the launcher UI. Deliberately dependency-free
 * — the app has only a few dozen strings, so a small dictionary plus a `t()`
 * helper (mirroring the Volt panel's own i18n) beats pulling in a full i18n
 * library.
 *
 * Language selection: a stored manual choice wins; otherwise the OS/browser
 * locale is auto-detected; otherwise Russian (the product is RU-first). The
 * manual choice is persisted in `localStorage` so it survives restarts.
 */
import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'

export type Lang = 'ru' | 'en'

const STORE_KEY = 'volt-admin-lang'

type Dict = Record<string, string>

const ru: Dict = {
  'app.title': 'Подключение',
  'app.subtitle': 'Выберите сохраненный профиль или добавьте новый.',
  'list.loading': 'Загрузка профилей…',
  'list.empty': 'Сохраненных профилей нет.',
  'mode.local': 'Локально',
  'mode.remote': 'По сети',
  'card.panelPort': 'порт панели {port}',
  'btn.edit': 'Изменить',
  'btn.delete': 'Удалить',
  'btn.connect': 'Подключить',
  'btn.cancel': 'Отмена',
  'btn.login': 'Войти',
  'btn.save': 'Сохранить',
  'btn.addProfile': '+ Добавить профиль',
  'pwd.placeholder': 'Пароль SSH',
  'form.editTitle': 'Изменить профиль',
  'form.newTitle': 'Новый профиль',
  'field.name': 'Название',
  'field.name.ph': 'Мой сервер',
  'field.host': 'Хост',
  'field.sshPort': 'SSH-порт',
  'field.user': 'Пользователь',
  'field.auth': 'Способ входа',
  'auth.agent': 'Ключ (по умолчанию)',
  'auth.keyFile': 'Файл ключа…',
  'auth.password': 'Пароль',
  'auth.hint': 'Ключ по умолчанию берется из ssh-agent/~/.ssh. Пароль — только если ключа нет.',
  'field.keyPath': 'Путь к файлу ключа',
  'field.panelPort': 'Порт панели',
  'field.principal': 'Принципал (администратор)',
  'field.principal.hint': 'Идентификатор из [[authz.principals]] с профилем администратора; код сопряжения выпускается на него.',
  'field.paircodeCommand': 'Команда кода сопряжения',
  'field.paircodeCommand.hint': 'Выполняется на машине с voltd (локально или по SSH) и печатает одноразовый код. Для voltd в Docker: docker exec voltd voltd --config-dir /voltd-data/.voltd gateway get-paircode --new --port 42617 --principal admin',
  'err.load': 'Не удалось загрузить профили: {e}',
  'err.nameRequired': 'Введите название профиля',
  'err.save': 'Не удалось сохранить профиль: {e}',
  'err.delete': 'Не удалось удалить профиль: {e}',
  'err.connect': 'Не удалось подключиться: {e}',
}

const en: Dict = {
  'app.title': 'Connect',
  'app.subtitle': 'Choose a saved profile or add a new one.',
  'list.loading': 'Loading profiles…',
  'list.empty': 'No saved profiles.',
  'mode.local': 'Local',
  'mode.remote': 'Remote',
  'card.panelPort': 'panel port {port}',
  'btn.edit': 'Edit',
  'btn.delete': 'Delete',
  'btn.connect': 'Connect',
  'btn.cancel': 'Cancel',
  'btn.login': 'Sign in',
  'btn.save': 'Save',
  'btn.addProfile': '+ Add profile',
  'pwd.placeholder': 'SSH password',
  'form.editTitle': 'Edit profile',
  'form.newTitle': 'New profile',
  'field.name': 'Name',
  'field.name.ph': 'My server',
  'field.host': 'Host',
  'field.sshPort': 'SSH port',
  'field.user': 'User',
  'field.auth': 'Sign-in method',
  'auth.agent': 'Key (default)',
  'auth.keyFile': 'Key file…',
  'auth.password': 'Password',
  'auth.hint': 'By default the key comes from ssh-agent / ~/.ssh. A password is used only if there is no key.',
  'field.keyPath': 'Key file path',
  'field.panelPort': 'Panel port',
  'field.principal': 'Principal (administrator)',
  'field.principal.hint': 'An id from [[authz.principals]] with an administrator profile; the pairing code is minted for it.',
  'field.paircodeCommand': 'Pairing-code command',
  'field.paircodeCommand.hint': 'Runs on the machine with voltd (locally or over SSH) and prints a one-time code. For voltd in Docker: docker exec voltd voltd --config-dir /voltd-data/.voltd gateway get-paircode --new --port 42617 --principal admin',
  'err.load': 'Failed to load profiles: {e}',
  'err.nameRequired': 'Enter a profile name',
  'err.save': 'Failed to save profile: {e}',
  'err.delete': 'Failed to delete profile: {e}',
  'err.connect': 'Failed to connect: {e}',
}

const messages: Record<Lang, Dict> = { ru, en }

/** Resolve the initial language: stored choice → OS/browser locale → RU. */
export function detectLang(): Lang {
  try {
    const stored = localStorage.getItem(STORE_KEY)
    if (stored === 'ru' || stored === 'en') return stored
  } catch {
    /* localStorage may be unavailable — fall through to auto-detect */
  }
  const nav = (typeof navigator !== 'undefined' ? navigator.language : '') || ''
  if (nav.toLowerCase().startsWith('en')) return 'en'
  // RU-first default: Russian for ru* and anything ambiguous.
  return 'ru'
}

export type Translate = (key: string, params?: Record<string, string | number>) => string

interface I18nContext {
  lang: Lang
  setLang: (l: Lang) => void
  t: Translate
}

const Ctx = createContext<I18nContext | null>(null)

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectLang)

  useEffect(() => {
    try {
      document.documentElement.lang = lang
    } catch {
      /* non-DOM environment — ignore */
    }
  }, [lang])

  const setLang = (l: Lang) => {
    setLangState(l)
    try {
      localStorage.setItem(STORE_KEY, l)
    } catch {
      /* persistence best-effort */
    }
  }

  const t: Translate = (key, params) => {
    let s = messages[lang][key] ?? messages.ru[key] ?? key
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        s = s.split(`{${k}}`).join(String(v))
      }
    }
    return s
  }

  return <Ctx.Provider value={{ lang, setLang, t }}>{children}</Ctx.Provider>
}

export function useI18n(): I18nContext {
  const ctx = useContext(Ctx)
  if (!ctx) {
    throw new Error('useI18n must be used within a LanguageProvider')
  }
  return ctx
}
