#!/usr/bin/env perl
# Generates a small mysqldump in the libgen.li "libgen_new" format for local testing.
# Usage: perl scripts/gen-sample-dump.pl > sample.sql
use strict;
use warnings;

my (%cols, @order, $cur);
while (my $l = <DATA>) {
    if ($l =~ /^CREATE TABLE `(\w+)`/) { $cur = $1; push @order, $cur; $cols{$cur} = [] }
    elsif ($cur && $l =~ /^  `(\w+)`/) { push @{ $cols{$cur} }, $1 }
    elsif ($l =~ /^\)/) { $cur = undef }
}
seek DATA, 0, 0;
my $schema = do { local $/; <DATA> };
$schema =~ s/.*?__DATA__\n//s;
my %create = map { $_ => ($schema =~ /(CREATE TABLE `$_` \(.*?\n\)[^\n]*\n)/s)[0] } @order;

sub sqlq { my $v = shift; return 'NULL' unless defined $v; return $v if $v =~ /^-?\d+$/; $v =~ s/\\/\\\\/g; $v =~ s/'/\\'/g; "'$v'" }
sub insert {
    my ($t, @rows) = @_;
    return '' unless @rows;
    my @tuples = map { my $r = $_; '(' . join(',', map { sqlq(exists $r->{$_} ? $r->{$_} : '') } @{ $cols{$t} }) . ')' } @rows;
    "INSERT INTO `$t` VALUES " . join(',', @tuples) . ";\n";
}

# title | author | year | publisher | series | language | isbn | formats (ext:sizeKB,...)
my @books = map { [split /\s*\|\s*/] } grep { /\S/ } split /\n/, <<'BOOKS';
The Hobbit | Tolkien, J. R. R. | 1937 | George Allen & Unwin | Middle-earth | English | 9780261103283 | epub:520,mobi:890,pdf:4200,djvu:9800
The Fellowship of the Ring | Tolkien, J. R. R. | 1954 | HarperCollins | The Lord of the Rings | English | 9780261103573 | epub:980,azw3:1300,pdf:3100
The Two Towers | Tolkien, J. R. R. | 1954 | HarperCollins | The Lord of the Rings | English | 9780261103580 | epub:910,pdf:2900
The Return of the King | Tolkien, J. R. R. | 1955 | HarperCollins | The Lord of the Rings | English | 9780261103597 | epub:940,mobi:1200
The Hobbit (SparkNotes Study Guide) | SparkNotes | 2014 | SparkNotes | | English | | pdf:300
Dune | Herbert, Frank | 1965 | Chilton Books | Dune | English | 9780441013593 | epub:1100,mobi:1500,pdf:2600
Dune Messiah | Herbert, Frank | 1969 | Putnam | Dune | English | 9780441172696 | epub:600
Children of Dune | Herbert, Frank | 1976 | Putnam | Dune | English | 9780441104024 | epub:720,fb2:900
Foundation | Asimov, Isaac | 1951 | Gnome Press | Foundation | English | 9780553293357 | epub:410,azw3:560,txt:520
I, Robot | Asimov, Isaac | 1950 | Gnome Press | Robot | English | 9780553382563 | epub:380
Neuromancer | Gibson, William | 1984 | Ace | Sprawl | English | 9780441569595 | epub:450,pdf:1800
Pride and Prejudice | Austen, Jane | 1813 | T. Egerton | | English | 9780141439518 | epub:620,mobi:700,pdf:1500,txt:690
Emma | Austen, Jane | 1815 | John Murray | | English | 9780141439587 | epub:710
Crime and Punishment | Dostoevsky, Fyodor | 1866 | The Russian Messenger | | English | 9780143058144 | epub:830,pdf:2400
Преступление и наказание | Достоевский, Фёдор | 1866 | Русский вестник | | Russian | | fb2:900,epub:870
Der Prozess | Kafka, Franz | 1925 | Die Schmiede | | German | 9783596294312 | epub:350
Le Petit Prince | Saint-Exupéry, Antoine de | 1943 | Gallimard | | French | 9782070612758 | epub:5200,pdf:8400
The Name of the Wind | Rothfuss, Patrick | 2007 | DAW Books | The Kingkiller Chronicle | English | 9780756404741 | epub:1500,mobi:1900,azw3:2100
The Wise Man's Fear | Rothfuss, Patrick | 2011 | DAW Books | The Kingkiller Chronicle | English | 9780756407124 | epub:1900
A Game of Thrones | Martin, George R. R. | 1996 | Bantam | A Song of Ice and Fire | English | 9780553103540 | epub:2100,mobi:2600,pdf:5100
The Martian | Weir, Andy | 2011 | Crown | | English | 9780553418026 | epub:700,azw3:900
Project Hail Mary | Weir, Andy | 2021 | Ballantine | | English | 9780593135204 | epub:3400,pdf:12000
Harry Potter and the Philosopher's Stone | Rowling, J. K. | 1997 | Bloomsbury | Harry Potter | English | 9780747532699 | epub:1100,mobi:1300,pdf:2400
The Great Gatsby | Fitzgerald, F. Scott | 1925 | Scribner | | English | 9780743273565 | epub:280,txt:300
1984 | Orwell, George | 1949 | Secker & Warburg | | English | 9780451524935 | epub:420,mobi:520,pdf:980
Animal Farm | Orwell, George | 1945 | Secker & Warburg | | English | 9780451526342 | epub:190
The Road | McCarthy, Cormac | 2006 | Knopf | | English | 9780307387899 | epub:360
Snow Crash | Stephenson, Neal | 1992 | Bantam | | English | 9780553380958 | epub:890,pdf:2100
The Hitchhiker's Guide to the Galaxy | Adams, Douglas | 1979 | Pan Books | Hitchhiker's Guide | English | 9780345391803 | epub:330,mobi:410
Mistborn: The Final Empire | Sanderson, Brandon | 2006 | Tor | Mistborn | English | 9780765311788 | epub:1700,azw3:2000
The Science of Dune | Grazier, Kevin R. | 2008 | BenBella Books | | English | 9781933771281 | epub:900
Sand Dunes: Geology and Ecology of Desert Dunes | Lancaster, Nicholas | 1995 | Routledge | | English | 9780415060943 | pdf:12000
Dune: The Graphic Novel, Book 1 | Herbert, Frank; Herbert, Brian | 2020 | Abrams ComicArts | Dune | English | 9781419731495 | pdf:98000,cbz:120000
The Martian Chronicles | Bradbury, Ray | 1950 | Doubleday | | English | 9781451678192 | epub:400
On the Road | Kerouac, Jack | 1957 | Viking Press | | English | 9780140283297 | epub:500
The Road to Wigan Pier | Orwell, George | 1937 | Victor Gollancz | | English | 9780141185293 | epub:420
Summary of Project Hail Mary by Andy Weir | QuickRead | 2021 | | | English | | epub:120
Hobbit Houses: Building Underground Homes | Smith, Paul | 2015 | Earth Press | | English | | pdf:8000
BOOKS

my (@ed, @descr, @etf, @files);
my ($e, $f, $d, $l) = (1000, 5000, 1, 1);
for my $b (@books) {
    my ($title, $author, $year, $pub, $series, $lang, $isbn, $formats) = @$b;
    $e++;
    push @ed, { e_id => $e, libgen_topic => 'f', title => $title, author => $author, year => $year, publisher => $pub, series_name => $series // '', visible => '', time_added => '2024-01-01 00:00:00', time_last_modified => '2024-01-01 00:00:00' };
    push @descr, { e_add_id => $d++, e_id => $e, key => 101, value => $lang };
    push @descr, { e_add_id => $d++, e_id => $e, key => 505, value => $isbn } if $isbn;
    push @descr, { e_add_id => $d++, e_id => $e, key => 305, value => "A long annotation for $title " . ('lorem ipsum ' x 20) };
    for my $fmt (split /,/, $formats) {
        my ($ext, $kb) = split /:/, $fmt;
        $f++;
        my $md5 = substr(unpack('H*', pack('N', $f) . $title . $ext) . ('0' x 32), 0, 32);
        push @files, { f_id => $f, md5 => $md5, extension => $ext, filesize => $kb * 1024, pages => ($ext =~ /pdf|djvu/ ? 320 : 0), visible => '', broken => 'N', libgen_topic => 'f', fiction_id => $f - 5000, time_added => '2024-01-01 00:00:00', time_last_modified => '2024-01-01 00:00:00' };
        push @etf, { etf_id => $l++, f_id => $f, e_id => $e };
    }
}
# A scientific article (topic a): must be skipped when a source only covers fiction/non-fiction.
push @ed, { e_id => 9990, libgen_topic => 'a', title => 'On the Quantum Hobbit Effect', author => 'Physicist, A.', visible => '' };
push @files, { f_id => 9990, md5 => 'd' x 32, extension => 'pdf', filesize => 100000, visible => '', broken => 'N', libgen_topic => 'a' };
push @etf, { etf_id => $l++, f_id => 9990, e_id => 9990 };

# Hidden edition, broken file and a non-ebook archive: all must be skipped by the indexer.
push @ed, { e_id => 9999, title => 'Hidden Book', author => 'Nobody', visible => 'del' };
push @files, { f_id => 9998, md5 => 'b' x 32, extension => 'epub', filesize => 1, visible => '', broken => 'Y' };
push @files, { f_id => 9997, md5 => 'c' x 32, extension => 'zip', filesize => 1, visible => '' };
push @etf, { etf_id => $l++, f_id => 9998, e_id => 1001 }, { etf_id => $l++, f_id => 9997, e_id => 1001 };

my @keys = ({ key => 101, name_en => 'Language' }, { key => 505, name_en => 'ISBN' }, { key => 305, name_en => 'Annotation' });

print "-- MySQL dump 10.13 (bookjev sample)\n/*!40101 SET NAMES utf8 */;\nUSE `libgen_new`;\n\n";
my %rows = (editions => \@ed, editions_add_descr => \@descr, editions_to_files => \@etf, elem_descr => \@keys, files => \@files);
for my $t (@order) {
    print "DROP TABLE IF EXISTS `$t`;\n", $create{$t}, "\nLOCK TABLES `$t` WRITE;\n";
    my @r = @{ $rows{$t} };
    while (my @chunk = splice @r, 0, 25) { print insert($t, @chunk) }
    print "UNLOCK TABLES;\n\n";
}
__DATA__
CREATE TABLE `editions` (
  `e_id` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `libgen_topic` enum('a','s','l','f','r','m','c') COLLATE utf8mb4_unicode_ci NOT NULL COMMENT 'раздел LG',
  `type` varchar(50) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Тип издания',
  `series_name` varchar(500) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `title` varchar(2000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Заголовок',
  `title_add` varchar(200) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Дополнение к заглавию',
  `author` varchar(2000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `publisher` varchar(1000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `city` varchar(200) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Город',
  `edition` varchar(250) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `year` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Год',
  `month` enum('','1q','1s','1t','2q','2s','2t','3q','3t','4q','4t','apr','aug','chr','dec','fal','feb','hol','jan','jul','jun','mar','may','mon','nov','oct','sep','spr','sum','win','aut') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `day` varchar(2) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'День издания',
  `pages` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `editions_add_info` varchar(500) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Библиографический комментарий к изданию (вид выпуска -  спец., ежегодник (если не выделены  в отдельную подшивку))',
  `cover_url` varchar(450) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Ссылка на обложку',
  `cover_exists` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Наличие обложки в репозитории lg editions',
  `issue_s_id` int(11) NOT NULL DEFAULT '0' COMMENT 'Ссылка на таблицу series для периодических изданий',
  `issue_number_in_year` int(10) unsigned NOT NULL DEFAULT '0' COMMENT 'Техническая нумерация в году для сортировки',
  `issue_year_number` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Номер за год',
  `issue_number` varchar(95) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Номер выпуска (в рамках тома)',
  `issue_volume` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Том',
  `issue_split` int(10) unsigned NOT NULL DEFAULT '0' COMMENT 'Признак того, что номер сдвоен, 0-не сдвоен, 1,2,3 - с каким числом номеров сдвоен',
  `issue_total_number` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Сквозная нумерация всей подшивки',
  `issue_first_page` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `issue_last_page` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `issue_year_end` varchar(4) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Конечный год, заполняется если номер сдвоенный или приходится на границу 2-х годов',
  `issue_month_end` enum('','1q','1s','1t','2q','2s','2t','3q','3t','4q','4t','apr','aug','chr','dec','fal','feb','hol','jan','jul','jun','mar','may','mon','nov','oct','sep','spr','sum','win','aut') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `issue_day_end` varchar(2) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Конечный день, заполняется если номер сдвоенный или приходится на границу 2-х годов',
  `issue_closed` int(1) unsigned NOT NULL DEFAULT '0' COMMENT 'Если номер нe издавался 0, иначе=1',
  `doi` varchar(200) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `time_added` timestamp NOT NULL DEFAULT '0000-00-00 00:00:00' COMMENT 'Дата добавления',
  `time_last_modified` timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT 'Дата последнего изменения',
  `visible` varchar(3) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Видимое, или закрыто для просмотра по разным причинам',
  `editable` tinyint(1) NOT NULL DEFAULT '1' COMMENT 'Возможность редактирования пользователями',
  `uid` int(10) unsigned NOT NULL DEFAULT '0',
  `commentary` varchar(200) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  PRIMARY KEY (`e_id`) USING BTREE,
  KEY `YEAR` (`year`),
  KEY `N_YEAR` (`issue_number_in_year`),
  KEY `MONTH` (`month`),
  KEY `MONTH_END` (`issue_month_end`),
  KEY `VISIBLE` (`visible`),
  KEY `LG_TOP` (`libgen_topic`),
  KEY `TYPE` (`type`),
  KEY `COMMENT` (`commentary`),
  KEY `S_ID` (`issue_s_id`),
  KEY `DOI` (`doi`) USING BTREE,
  KEY `ISSUE` (`issue_number`,`issue_volume`,`issue_year_number`,`issue_total_number`) USING BTREE,
  KEY `DAY` (`day`),
  KEY `TIME` (`time_added`) USING BTREE,
  KEY `TIMELM` (`time_last_modified`)
) ENGINE=MyISAM AUTO_INCREMENT=208971839 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='Издания, в.т.ч. периодические';

CREATE TABLE `editions_add_descr` (
  `e_add_id` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `e_id` int(10) unsigned NOT NULL DEFAULT '0',
  `key` int(10) unsigned NOT NULL DEFAULT '0' COMMENT 'Ссылка на описание elem_descr ',
  `value` mediumtext COLLATE utf8mb4_unicode_ci,
  `value_add1` mediumtext COLLATE utf8mb4_unicode_ci,
  `value_add2` mediumtext COLLATE utf8mb4_unicode_ci,
  `value_add3` mediumtext COLLATE utf8mb4_unicode_ci,
  `value_hash` bigint(20) unsigned NOT NULL,
  `date_start` date DEFAULT NULL,
  `date_end` date DEFAULT NULL,
  `issue_start` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Начальное издание, при наличие issue_able в elem_descr',
  `issue_end` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Конечное издание, при наличие issue_able в elem_descr',
  `time_added` timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  `time_last_modified` timestamp NOT NULL DEFAULT '0000-00-00 00:00:00',
  `commentary` varchar(1000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `uid` int(11) DEFAULT '0',
  `value_id` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`e_add_id`) USING BTREE,
  UNIQUE KEY `VAL_UNIQ` (`value_hash`,`e_id`,`key`),
  KEY `KEY` (`key`),
  KEY `TIME` (`time_added`,`time_last_modified`) USING BTREE,
  KEY `VAL3` (`value_add3`(50)),
  KEY `VAL` (`value`(50)),
  KEY `VAL2` (`value_add2`(50)),
  KEY `VAL1` (`value_add1`(50)),
  KEY `VAL_ID` (`value_id`) USING BTREE,
  KEY `EID` (`e_id`) USING BTREE
) ENGINE=MyISAM AUTO_INCREMENT=295351197 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='Дополнительные элементы описания к изданиям';

CREATE TABLE `editions_to_files` (
  `etf_id` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `f_id` int(10) unsigned NOT NULL,
  `e_id` int(10) unsigned NOT NULL,
  `time_added` datetime NOT NULL,
  `time_last_modified` datetime NOT NULL,
  `uid` int(10) unsigned NOT NULL DEFAULT '0',
  PRIMARY KEY (`etf_id`) USING BTREE,
  UNIQUE KEY `IDS` (`f_id`,`e_id`),
  KEY `TIME` (`time_added`,`time_last_modified`),
  KEY `FID` (`f_id`),
  KEY `EID` (`e_id`)
) ENGINE=MyISAM AUTO_INCREMENT=118132637 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE `elem_descr` (
  `key` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `commentary` varchar(1000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Комментарий',
  `name_ru` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на русском - зависит от языка интерфейса',
  `name_en` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на английском',
  `type` varchar(3) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'тип данных - гиперссылка, xml, ссылка на картинку и пр.',
  `checks` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'проверка значения через регулярные выражения или ссылки на справочники',
  `link_pattern` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Гиперссылка для дополнения id- ссылки на справочник',
  `name_add1_ru` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на русском - зависит от языка интерфейса',
  `name_add1_en` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на английском',
  `type_add1` varchar(3) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'тип данных - гиперссылка, xml, ссылка на картинку и пр.',
  `checks_add1` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'проверка значения через регулярные выражения или ссылки на справочники',
  `filled_add1` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Обязательность заполнения',
  `link_pattern_add1` varchar(50) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Гиперссылка для дополнения id- ссылки на справочник',
  `name_add2_ru` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на русском - зависит от языка интерфейса',
  `name_add2_en` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на английском',
  `type_add2` varchar(3) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'тип данных - гиперссылка, xml, ссылка на картинку и пр.',
  `checks_add2` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'проверка значения через регулярные выражения или ссылки на справочники',
  `filled_add2` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Обязательность заполнения',
  `link_pattern_add2` varchar(50) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Гиперссылка для дополнения id- ссылки на справочник',
  `name_add3_ru` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на русском - зависит от языка интерфейса',
  `name_add3_en` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наименование описательного элемента на английском',
  `type_add3` varchar(3) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'тип данных - гиперссылка, xml, ссылка на картинку и пр.',
  `checks_add3` varchar(100) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'проверка значения через регулярные выражения или ссылки на справочники',
  `filled_add3` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Обязательность заполнения',
  `link_pattern_add3` varchar(50) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Гиперссылка для дополнения id- ссылки на справочник',
  `for_works` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для работ',
  `for_publishers` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для издательств',
  `for_editions` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для изданий',
  `for_authors` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для авторов',
  `for_series` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'для серий',
  `for_files` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для файлов',
  `dateable` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Может ли иметь период действия с - по',
  `issueable` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Может ли иметь период действия с выпуска - по выпуск',
  `default_view_for_edit` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Показывать по умолчанию при редактировании',
  `multiple_values` tinyint(1) NOT NULL DEFAULT '1' COMMENT 'у объекта может быть несколько описательных полей с одним и тем же типом',
  `for_libgen` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `for_fiction` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `for_fiction_rus` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `for_scimag` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `for_magz` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `for_standarts` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `for_comics` tinyint(1) NOT NULL DEFAULT '0' COMMENT 'Для раздела',
  `sort` int(11) NOT NULL DEFAULT '0' COMMENT 'Сортировка',
  `visible` tinyint(1) NOT NULL DEFAULT '1' COMMENT 'Видимое в описани',
  `editable` tinyint(1) NOT NULL DEFAULT '1' COMMENT 'Возможно ручное редактирование пользователем',
  PRIMARY KEY (`key`) USING BTREE,
  UNIQUE KEY `UNIQ1` (`name_ru`),
  UNIQUE KEY `UNIQ2` (`name_en`),
  KEY `key` (`key`,`type`,`type_add1`,`type_add2`,`type_add3`,`for_works`,`for_publishers`,`for_editions`,`for_authors`,`for_series`,`for_files`,`multiple_values`,`issueable`,`dateable`)
) ENGINE=MyISAM AUTO_INCREMENT=1001 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='Виды элементов описания';

CREATE TABLE `files` (
  `f_id` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `md5` varchar(32) COLLATE utf8mb4_unicode_ci NOT NULL,
  `pages` int(10) unsigned NOT NULL DEFAULT '0' COMMENT 'Техническое количество страниц в скане',
  `dpi` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Разрешение',
  `visible` varchar(3) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Видимый, если не пусто, то указывает по каким причинам - cpr -  абуза, del- удален физически, no - прочие причины',
  `time_added` datetime NOT NULL,
  `time_last_modified` datetime NOT NULL,
  `cover_url` varchar(255) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Ссылка на обложку',
  `cover_exists` tinyint(1) NOT NULL DEFAULT '0',
  `commentary` varchar(1000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Доп. инфо о скане (fixed и пр.)',
  `color` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Цветной',
  `cleaned` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Очищенный скан',
  `orientation` enum('P','L','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Ориентация скана - Портретная, Ландшафтная',
  `paginated` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Разворот разрезан на страницы',
  `scanned` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Сканированный',
  `vector` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Векторный',
  `bookmarked` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Есть оглавление',
  `ocr` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Есть текстовый слой',
  `filesize` bigint(20) unsigned NOT NULL DEFAULT '0' COMMENT 'Размер файла',
  `extension` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Расширение',
  `locator` varchar(500) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Имя файла (до загрузки  в репозиторий)',
  `broken` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Битый',
  `editable` tinyint(1) unsigned NOT NULL DEFAULT '1' COMMENT 'Запись редактируема',
  `generic` char(32) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Ссылка на лучшую версию файла',
  `cover_info` varchar(200) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Информация об обложках (если их несколько)',
  `file_create_date` datetime NOT NULL DEFAULT '2000-01-01 05:00:00' COMMENT 'Техническая дата создания файла',
  `archive_files_count` int(10) unsigned NOT NULL DEFAULT '0',
  `archive_dop_files_flag` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'наличие доп. файлов кроме картинок, для cbr, cbz, rar, zip, 7z',
  `archive_files_pic_count` int(10) unsigned NOT NULL DEFAULT '0' COMMENT 'Количество картинок в архиве',
  `scan_type` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Тип скана - цифровой, веб, бумажный скан, микропленка',
  `scan_content` varchar(145) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `c2c` enum('Y','N','') COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Наличие рекламы в скане (c2c)',
  `scan_quality` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Качество скана (HQ, Q10)',
  `releaser` varchar(125) COLLATE utf8mb4_unicode_ci DEFAULT '' COMMENT 'Автор релиза',
  `version` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'Версия',
  `nfo` tinyint(3) unsigned NOT NULL DEFAULT '0' COMMENT 'Есть NFO файл',
  `sfv` tinyint(3) unsigned NOT NULL DEFAULT '0' COMMENT 'Есть SFV файл',
  `diz` tinyint(3) unsigned NOT NULL DEFAULT '0' COMMENT 'Есть DIZ файл',
  `fbd` tinyint(3) unsigned NOT NULL DEFAULT '0' COMMENT 'Есть FBD файл',
  `libgen_id` int(10) unsigned NOT NULL DEFAULT '0',
  `fiction_id` int(10) unsigned NOT NULL DEFAULT '0',
  `fiction_rus_id` int(10) unsigned NOT NULL DEFAULT '0',
  `comics_id` int(10) unsigned NOT NULL DEFAULT '0',
  `scimag_id` int(10) unsigned NOT NULL DEFAULT '0',
  `standarts_id` int(10) unsigned NOT NULL DEFAULT '0',
  `magz_id` int(10) unsigned NOT NULL DEFAULT '0',
  `libgen_topic` enum('l','s','m','c','f','r','a') COLLATE utf8mb4_unicode_ci NOT NULL COMMENT 'правильный раздел для файла',
  `scan_size` varchar(45) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '' COMMENT 'размер рандомной картинки из архива',
  `scimag_archive_path` varchar(1000) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT '',
  `scimag_archive_path_is_doi` tinyint(1) DEFAULT '0' COMMENT 'Путь в архиве соответствует doi в editions',
  `uid` int(10) unsigned NOT NULL DEFAULT '0',
  PRIMARY KEY (`f_id`),
  UNIQUE KEY `MD5_UNIQ` (`md5`) USING BTREE,
  KEY `MAGZID` (`magz_id`),
  KEY `COMICSID` (`comics_id`),
  KEY `LGTOPIC` (`libgen_topic`),
  KEY `FICID` (`fiction_id`),
  KEY `FICTRID` (`fiction_rus_id`),
  KEY `SMID` (`scimag_id`),
  KEY `STDID` (`standarts_id`),
  KEY `LGID` (`libgen_id`),
  KEY `FSIZE` (`filesize`),
  KEY `TIME` (`time_added`) USING BTREE,
  KEY `TIMELM` (`time_last_modified`) USING BTREE
) ENGINE=MyISAM AUTO_INCREMENT=116487350 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='Файлы';

