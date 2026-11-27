import './polyfills.js';
import * as cheerio from 'cheerio';
import { Parser } from 'htmlparser2';
import dayjs from 'dayjs';
import { install } from './glue.js';

install({ cheerio, Parser, dayjs });
