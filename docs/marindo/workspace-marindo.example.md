# Workspace Marindo (Plane) - contoh teredaksi

Versi yang aman dibagikan dari `workspace-marindo.md`. File aslinya memuat nama
karyawan, email, NIP, dan tautan internal dari dump produksi, jadi gitignored.
File ini tetap memuat seluruh card beserta deskripsi dan komentar, tetapi data
pribadi diganti placeholder.

Teks deskripsi dan komentar di bawah dikutip **verbatim** dari database, termasuk
salah tulis dan spasi aslinya. Yang berubah hanya token PII di tabel legenda.

## Legenda redaksi

| Placeholder                        | Menggantikan                                          |
| ---------------------------------- | ----------------------------------------------------- |
| `<user-a>`                         | admin yang aktif di card timesheet                    |
| `<user-b>`, `<user-c>`, `<user-d>` | admin lain tanpa aktivitas card                       |
| `<user-e>`                         | product owner, membuka card payroll dan SOP           |
| `<user-f>`                         | developer web pada card kategori timesheet            |
| `<user-g>`                         | developer pada card timesheet, towing, dan SOP        |
| `<hrd-contact>`                    | staf HRD yang dikutip di komentar                     |
| `<employee-name>`                  | nama crew yang disebut di komentar                    |
| `<nip-1>`, `<nip-2>`               | nomor induk pegawai yang dikutip di komentar          |
| `<nip-prefix>`                     | awalan nomor induk pegawai                            |
| `<internal-app-host>`              | host staging internal untuk halaman sounding dan fuel |
| `<drive-link>`                     | tautan Google Drive berisi diagram                    |

| Item         | Nilai                                              |
| ------------ | -------------------------------------------------- |
| Server       | `<ssh-host>`, container `plane-app-plane-db-1`     |
| Dump         | `<path-to>.sql` (sha256 `<hash>`)                  |
| DB lokal     | WSL Ubuntu, PostgreSQL 16, `127.0.0.1:5432`        |
| Workspace id | `5329d1e5-6fb8-4991-8d4f-994286e74c87`             |
| Slug         | `marindo`                                          |
| Diambil      | 2026-11-27, read-only dari hasil restore `pg_dump` |

## Ringkasan

| Metrik            | Jumlah                                                 |
| ----------------- | ------------------------------------------------------ |
| Project aktif     | 2 (`ALLTASK`, `SOP`)                                   |
| Project terhapus  | 1 (`TES`)                                              |
| Card aktif        | 9 (8 di `ALLTASK`, 1 di `SOP`)                         |
| Card terhapus     | 1 (`ALLTASK-4`)                                        |
| Komentar          | 13 (`ALLTASK-2` = 4, `ALLTASK-3` = 5, `ALLTASK-5` = 4) |
| Label             | 0                                                      |
| Cycle             | 0                                                      |
| Module            | 0                                                      |
| Page / dokumen    | 0                                                      |
| Anggota workspace | 7 (semua role `20` = Admin)                            |

## Project

| Identifier | Nama     | Status              | Card | Catatan                                |
| ---------- | -------- | ------------------- | ---- | -------------------------------------- |
| `ALLTASK`  | All Task | aktif               | 8    | Backlog operasional utama              |
| `SOP`      | sop      | aktif               | 1    | Masih placeholder                      |
| `TES`      | Tes      | terhapus 2026-11-23 | 0    | Project uji coba, card sudah tidak ada |

Alur state kedua project aktif: Backlog, Todo, In Progress, Done, Cancelled,
Triage.

## Anggota

| Nama       | Email                  | Role       |
| ---------- | ---------------------- | ---------- |
| `<user-a>` | `<user-a>@example.com` | 20 (Admin) |
| `<user-b>` | `<user-b>@example.com` | 20 (Admin) |
| `<user-c>` | `<user-c>@example.com` | 20 (Admin) |
| `<user-d>` | `<user-d>@example.com` | 20 (Admin) |
| `<user-e>` | `<user-e>@example.com` | 20 (Admin) |
| `<user-f>` | `<user-f>@example.com` | 20 (Admin) |
| `<user-g>` | `<user-g>@example.com` | 20 (Admin) |

Pemetaan role, observasi di `apps/api/plane/db/models/workspace.py:19`:
`ROLE_CHOICES = ((20, "Admin"), (15, "Member"), (5, "Guest"))`.

## Card

### `ALLTASK-1` - penawaran aplikasi SOP

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Backlog                      |
| Dibuat oleh            | `<user-e>`, 2026-11-23 13:48 |
| Terakhir diubah        | 2026-11-23 13:48             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | -                            |
| Komentar               | 0                            |

Deskripsi: _(kosong)_

Catatan: card administratif untuk penawaran aplikasi SOP. Belum ada rincian
teknis.

### `ALLTASK-2` - perhitungan gaji pegawai mangkir

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Done                         |
| Dibuat oleh            | `<user-e>`, 2026-11-23 13:48 |
| Terakhir diubah        | 2026-11-26 16:45             |
| Selesai                | 2026-11-26 16:45             |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | -                            |
| Komentar               | 4                            |

Deskripsi: _(kosong, seluruh konteks ada di komentar)_

Komentar (verbatim):

1. **`<user-g>`, 2026-11-25 09:14** - "info dari user 11 nop2026 `<hrd-contact>` HRDIni ada abk yang posisinya tidak ada di kapal dan tidak ada pwngajuan cuti di kantor. Jadi dari kami mau stopkan gaji dari tangga 10 november - ybs kembali dikapal.Kalo untuk di menu absen (izin pay off / izin pay on / hadir / tidak hadir) gaji akan tetap ngalir. Kecuali saya me RESIGN kan orang yang bersangkutan.Takutnya jika saya me Resignkan langsung di program, takutnya eror di tim finance.Kalo dari tim finance di resignkan pak. Tapi kalo dari hrd nanti bakal buat kontrak baru dan `<nip-prefix>` baru lagikaryawan `<employee-name>` `<nip-1>` (keterangannya resign karna gaji bulan ini di hold) `<nip-2>` (keterangan aktif untuk uang towing)"
2. **`<user-g>`, 2026-11-26 11:14** - " tetap pakai nip lama dengan catatan nanti gajinya di nolkan,kemudian dibuat catatan untuk mengurangi hutang yg belum terpotong di bulan"
3. **`<user-g>`, 2026-11-26 11:23** - "tambah fitur kapal di absen laporan absen"
4. **`<user-e>`, 2026-11-26 16:45** - "catatan rapat tadi bisa di note disini, sebagai tanda kalau tidak ada fitur baru di case ini"

### `ALLTASK-3` - timesheet harus selesai baru bisa bayaran towing

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | In Progress                  |
| Dibuat oleh            | `<user-g>`, 2026-11-24 09:35 |
| Terakhir diubah        | 2026-11-25 07:20             |
| Selesai                | -                            |
| Tanggal mulai / target | 2026-11-18 / -               |
| Priority               | low                          |
| Assignee               | `<user-f>`                   |
| Komentar               | 5                            |

Deskripsi: _(kosong, semua konteks di komentar)_

Komentar (verbatim):

1. **`<user-g>`, 2026-11-24 10:38** - "saya ada buat data mahkota 5sudah buat data timesheet sounding https://`<internal-app-host>`/operasional/sounding/2117 dan peritungan bbmhttps://`<internal-app-host>`/operasional/fuel/2111 analisa saya si 2162 muncul di list walapun belum selesai agar admin meningatkan kapten untuk menyeselaikan data timesheet saya mau buat towing , relasi data buyback untuk dimuncul di towing tidak muncul listnya"
2. **`<user-f>`, 2026-11-24 10:57** - "Gk ada transaksi buyback"
3. **`<user-g>`, 2026-11-24 11:06** - "stus perhitungan bbm sdh validdi perhitungan towing sudah muncul, tapi masih ada 1 timesheet yg masih proses dan itu harusnya complet dulu baru bisa towing bayaransetelah di cek, tidak muncul karena beda tongkang"
4. **`<user-g>`, 2026-11-24 11:11** - "ini data time sheet"
5. **`<user-g>`, 2026-11-24 11:26** - "`<drive-link>` analisa diagram"

Catatan: attachment tidak tersimpan di DB (0 baris `issue_attachments`), jadi
file Drive hanya tersisa sebagai URL.

### `ALLTASK-4` - test (terhapus)

| Field       | Nilai                          |
| ----------- | ------------------------------ |
| State       | - (card dihapus)               |
| Dibuat oleh | -                              |
| Dihapus     | 2026-11-24 10:30 (soft delete) |
| Komentar    | 0                              |

Deskripsi: _(kosong)_

Catatan: card uji coba berjudul "test". Tidak dihitung sebagai pekerjaan aktif,
tetapi barisnya masih ada dengan `deleted_at` terisi.

### `ALLTASK-5` - Set Pilihan ketegori Kegiatan Time Sheet di mobile Jadi Static

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Backlog                      |
| Dibuat oleh            | `<user-f>`, 2026-11-26 12:26 |
| Terakhir diubah        | 2026-11-27 07:29             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | `<user-a>`, `<user-f>`       |
| Komentar               | 4                            |

Deskripsi (verbatim, satu item per baris):

> Tambat
> Berlayar
> Berlabuh
> Muat
> Bongkar
> Sandar
>
> Set pilihan kategori kegiatan timesheet jadi list diatas

Komentar (verbatim):

1. **`<user-e>`, 2026-11-26 14:37** - "biar admin yg sesuaikan konten nya. kasih centang aktif"
2. **`<user-e>`, 2026-11-26 15:31** - "di web juga jadi statis"
3. **`<user-a>`, 2026-11-27 05:18** - "di apk tetap perlu update, karena ada proses simpan data ke db lokal. fitur time sheet pakai konsep first offline. sehingga jika ada peprubahan logic di API, maka di apknya perlu di update juga logicnya."
4. **`<user-e>`, 2026-11-27 07:29** - "diatur juga langkah update fitur nya, jgn sampai saat update ada masalah krn implementasi belum 100%"

### `ALLTASK-6` - laporan absen untuk abk/crew kapal

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Todo                         |
| Dibuat oleh            | `<user-g>`, 2026-11-27 08:37 |
| Terakhir diubah        | 2026-11-27 09:38             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | -                            |
| Komentar               | 0                            |

Deskripsi (verbatim):

> hasil rapat kemarin kamis 26 nop 2026
> dari admin HRD minta dibuatkan laporan untuk absen abk
>
> kebutuhan datanya /tabelnya
> no
> NIP
> nama crew
> jabatan
> nama kapal
> Tanggal
> type (hadir, izin pay off, izin pay on, tidak hadir) sesuai absensi yg sudah ada

Catatan: laporan absensi crew dengan kolom `no`, `NIP`, `nama crew`, `jabatan`,
`nama kapal`, `Tanggal`, dan `type` (hadir / izin pay off / izin pay on /
tidak hadir).

### `ALLTASK-7` - [WEB] Kategori Kegiatan Timesheet Tambah is_active

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | In Progress                  |
| Dibuat oleh            | `<user-f>`, 2026-11-27 09:18 |
| Terakhir diubah        | 2026-11-27 09:43             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | `<user-f>`                   |
| Komentar               | 0                            |

Deskripsi (verbatim):

> Tambah field is_active. Lalu buat fungsi untuk ubah status. Di index tambahkan
> filter by status.

Catatan: terkait langsung dengan `ALLTASK-5`; kategori kegiatan sekarang perlu
bisa diaktifkan/nonaktifkan, bukan hanya daftar statis.

### `ALLTASK-8` - [WEB] Timesheet Filter Get Kategori Kegiatan Jadi Yang Aktif Aja

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Todo                         |
| Dibuat oleh            | `<user-f>`, 2026-11-27 09:19 |
| Terakhir diubah        | 2026-11-27 09:23             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | `<user-f>`                   |
| Komentar               | 0                            |

Deskripsi (verbatim):

> Saat edit list option kategori kegiatan harus menambahkan option lama misalkan
> option lama sudah disabled.

Catatan: saat edit, opsi kategori lama yang sudah di-disable tetap harus
ditampilkan agar data lama tidak kehilangan label.

### `ALLTASK-9` - [WEB] Timesheet Kategori Kegiatan Disable Create New From Select Input

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Todo                         |
| Dibuat oleh            | `<user-f>`, 2026-11-27 09:21 |
| Terakhir diubah        | 2026-11-27 09:21             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | `<user-f>`                   |
| Komentar               | 0                            |

Deskripsi: _(kosong)_

Catatan: judulnya menjelaskan tujuan, hapus kemampuan membuat kategori baru
langsung dari select input supaya kategori hanya dikelola dari halaman master.

### `SOP-1` - sop

| Field                  | Nilai                        |
| ---------------------- | ---------------------------- |
| State                  | Backlog                      |
| Dibuat oleh            | `<user-g>`, 2026-11-25 13:40 |
| Terakhir diubah        | 2026-11-25 13:40             |
| Selesai                | -                            |
| Tanggal mulai / target | -                            |
| Priority               | none                         |
| Assignee               | -                            |
| Komentar               | 0                            |

Deskripsi: _(kosong)_

Catatan: placeholder, belum ada isi.

## Pola yang terlihat (inference, bukan observasi)

- Pekerjaan aktif berpusat di **timesheet -> penggajian -> towing**: `ALLTASK-3`
  dan `ALLTASK-2` sama-sama berhenti di aturan kapan gaji atau bayaran boleh
  jalan.
- `ALLTASK-5`, `ALLTASK-7`, `ALLTASK-8`, dan `ALLTASK-9` adalah satu perubahan
  yang sama, dipecah per layer (mobile, web, filter) seputar kategori kegiatan
  statis plus `is_active`.
- `SOP` dan `ALLTASK-1` masih penawaran atau placeholder tanpa pekerjaan teknis.
- `ALLTASK-4` dan project `TES` adalah sisa uji coba, keduanya sudah soft-delete.
- Tidak ada label, cycle, module, atau page, jadi board ini dipakai sebagai
  daftar tugas datar, bukan proyek terstruktur.
